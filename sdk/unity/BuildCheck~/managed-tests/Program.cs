// Offline unit checks for the managed core (no node needed): `dotnet run`.
using System;
using System.Collections.Generic;
using Aurix.Net;
using Aurix.Net.Managed;

static class Program
{
    static int _fails;
    static void Check(bool ok, string what) { if (!ok) { _fails++; Console.WriteLine("FAIL " + what); } else Console.WriteLine("ok   " + what); }

    // 1-D mover: input byte = signed velocity, world is a wall at x = 10.
    static int Step(int x, byte[] input) { int v = input != null && input.Length > 0 ? (sbyte)input[0] : 0; return Math.Min(10, x + v); }
    static byte[] In(int v) => new[] { (byte)(sbyte)v };

    static void PredictorTests()
    {
        var p = new Predictor<int>(0, Step);
        // Perfect prediction: server agrees tick by tick, nothing is replayed.
        p.Record(1, In(2)); p.Record(2, In(2)); p.Record(3, In(2));
        Check(p.Predicted == 6 && p.Pending == 3, "predicted 3 inputs ahead");
        Check(!p.Reconcile(1, 2) && p.Pending == 2 && p.Predicted == 6, "confirming tick 1 keeps the prediction");
        Check(!p.Reconcile(3, 6) && p.Pending == 0 && p.Mispredictions == 0, "all confirmed, no misprediction");

        // Server disagrees (we were pushed back to 1 at tick 4): later inputs replay on top of it.
        int correctedFrom = -1, correctedTo = -1;
        p.OnCorrected += (was, now) => { correctedFrom = was; correctedTo = now; };
        p.Record(4, In(2)); p.Record(5, In(2)); p.Record(6, In(2)); // predicts 8, 10(wall), 10
        Check(p.Reconcile(4, 1) && p.Predicted == 5 && p.Pending == 2, "replayed 2 pending inputs on the server state (1 + 2 + 2)");
        Check(p.Mispredictions == 1 && correctedFrom == 8 && correctedTo == 1, "correction reported with predicted vs authoritative");

        // Stale / reordered state is ignored.
        Check(!p.Reconcile(3, 99) && p.Predicted == 5, "stale tick ignored");

        // Custom divergence threshold: small drift tolerated.
        var q = new Predictor<int>(0, Step, (a, b) => Math.Abs(a - b) > 1);
        q.Record(1, In(3)); q.Record(2, In(3));
        Check(!q.Reconcile(1, 2) && q.Predicted == 6, "within threshold: no replay");
        Check(q.Reconcile(2, 0) && q.Predicted == 0, "beyond threshold: replay");

        // Capacity: oldest in-flight inputs are dropped.
        var r = new Predictor<int>(0, Step, null, 4);
        for (uint t = 1; t <= 10; t++) r.Record(t, In(1));
        Check(r.Pending == 4 && r.Predicted == 10, "ring keeps the last 4 inputs");
        r.Reset(0); Check(r.Pending == 0 && r.Predicted == 0, "reset");
    }

    static string Hex(byte[] b) { var sb = new System.Text.StringBuilder(); foreach (var x in b) sb.Append(x.ToString("x2")); return sb.ToString(); }

    // Golden vectors shared with the Web SDK tests (sdk/web/test/sync.test.mjs): the same schema + values
    // must encode to the same bytes in C# and TypeScript.
    static void CodecVectors()
    {
        var schema = Schema.Parse("component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\ncomponent Stats {\n  hp: u16\n  name: string(16)\n  speed: f32\n  team: i8\n  flags: bits(5)\n  seed: u64\n}\n");
        var tr = schema.ByName("Transform"); var st = schema.ByName("Stats");
        var world = new SyncWorld(schema);
        var pos = new ComponentValue { Fields = new[] { new Value { X = 1.25f, Y = -2.5f, Z = 300.75f }, new Value { X = 0.1f, Y = 0.2f, Z = 0.3f, W = 0.9f } } };
        var stats = new ComponentValue { Fields = new[] { new Value { U = 4242 }, new Value { S = "zé" }, new Value { F = 3.5f }, new Value { I = -3 }, new Value { U = 21 }, new Value { U = 0x0123456789abcdefUL } } };
        Console.WriteLine("VECTOR writes " + Hex(world.EncodeWrites(new List<(ComponentDef, ComponentValue)> { (tr, pos), (st, stats) })));
        var w = new BitWriter(); w.WriteBits(0x1ff, 9); w.WriteInt(-77, 12); w.WriteQuantized(12.345f, -100, 100, 0.001f); w.WriteF32(1.5f); w.WriteBits(ulong.MaxValue, 64); w.WriteBool(true);
        Console.WriteLine("VECTOR bits " + Hex(w.Finish()));
        Console.WriteLine("VECTOR input " + Hex(SyncWorld.EncodeInput(0x01020304, new List<byte[]> { new byte[] { 1, 2 }, new byte[0], new byte[] { 9 } })));
        Console.WriteLine("VECTOR ack " + Hex(SyncWorld.EncodeAck(0xdeadbeef, 0x80000001)));
    }

    static void LockstepTests()
    {
        var sent = new List<(Channel, byte[])>();
        var ls = new LockstepClient((ch, b) => sent.Add((ch, b)));
        // adaptive delay: late inputs raise it at once, steady slack lowers it slowly
        ls.OnInputStats(-2, 2);
        Check(ls.InputDelay == 4 && ls.LateInputs == 2, "late by 2 -> delay 2 -> 4");
        ls.OnInputStats(3, 0); ls.OnInputStats(3, 0);
        Check(ls.InputDelay == 4, "two slack reports: unchanged");
        ls.OnInputStats(3, 0);
        Check(ls.InputDelay == 3, "third slack report: -1");
        ls.OnInputStats(1, 0); ls.OnInputStats(3, 0); ls.OnInputStats(3, 0); ls.OnInputStats(3, 0);
        Check(ls.InputDelay == 2, "streak resets on a tight report");
        ls.InputDelay = ls.InputDelayMax; ls.OnInputStats(-9, 1);
        Check(ls.InputDelay == ls.InputDelayMax, "capped at max");
        // the same frame through Receive
        Check(ls.Receive(new byte[] { 0x25, 0, 0, 0, 10, unchecked((byte)(sbyte)-1), 1 }) && ls.LastMargin == -1, "0x25 parsed");
        // dump request -> event -> SendDump frame on the reliable channel
        uint asked = 0; ls.OnDumpRequested += t => asked = t;
        ls.Receive(new byte[] { 0x26, 0, 0, 1, 0 });
        Check(asked == 256, "dump requested for tick 256");
        ls.SendDump(256, 0x1122334455667788UL, new byte[] { 7, 7 });
        var d = sent[sent.Count - 1];
        Check(d.Item1 == Channel.Reliable && Hex(d.Item2) == "27000001001122334455667788" + "0707", "dump frame layout");
        // input redundancy window
        ls.InputRedundancy = 2; ls.SendInput(new byte[] { 1 }); ls.SendInput(new byte[] { 2 }); ls.SendInput(new byte[] { 3 });
        Check(sent[sent.Count - 1].Item2[5] == 2, "two inputs per datagram");
    }

    static int Main()
    {
        PredictorTests();
        LockstepTests();
        CodecVectors();
        Console.WriteLine(_fails == 0 ? "all managed checks passed" : _fails + " check(s) failed");
        return _fails == 0 ? 0 : 1;
    }
}
