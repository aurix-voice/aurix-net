// Offline unit tests of the managed core (no node needed): `dotnet test`.
using System;
using System.Collections.Generic;
using System.Linq;
using Aurix.Net;
using Aurix.Net.Managed;
using Aurix.Net.Sim;
using Aurix.Net.Sim.Bots;
using System.Text.Json;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Net.WebSockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using NUnit.Framework;

[TestFixture]
public class ManagedCoreTests
{
    // 1-D mover: input byte = signed velocity, world is a wall at x = 10.
    static int Step(int x, byte[] input) { int v = input != null && input.Length > 0 ? (sbyte)input[0] : 0; return Math.Min(10, x + v); }
    static byte[] In(int v) => new[] { (byte)(sbyte)v };

    [Test]
    public void TickTimerTests()
    {
        var t = TickTimer.CreateFromSeconds(100, 0.5f, 60);
        Assert.That(t.IsRunning && t.End == 130 && !t.Expired(100) && t.IsActive(129) && t.RemainingTicks(110) == 20, "0.5 s at 60 Hz = 30 ticks");
        Assert.That(t.Expired(130) && t.Expired(500) && !t.IsActive(130) && t.RemainingTicks(130) == 0, "expired at End and stays expired");
        Assert.That(System.Math.Abs(t.Normalized(115) - 0.5f) < 1e-6 && t.Normalized(999) == 1f, "normalized progress");
        Assert.That(!TickTimer.None.IsRunning && !TickTimer.None.Expired(10) && TickTimer.None.Normalized(10) == 1f, "None never runs or expires");
        Assert.That(TickTimer.CreateFromTicks(5, 0).End == 6, "zero ticks still expires one tick later");
        Assert.That(System.Math.Abs(t.RemainingSeconds(100, 60) - 0.5f) < 1e-6, "remaining seconds");
    }

    [Test]
    public void PredictorTests()
    {
        var p = new Predictor<int>(0, Step);
        // Perfect prediction: server agrees tick by tick, nothing is replayed.
        p.Record(1, In(2)); p.Record(2, In(2)); p.Record(3, In(2));
        Assert.That(p.Predicted == 6 && p.Pending == 3, "predicted 3 inputs ahead");
        Assert.That(!p.Reconcile(1, 2) && p.Pending == 2 && p.Predicted == 6, "confirming tick 1 keeps the prediction");
        Assert.That(!p.Reconcile(3, 6) && p.Pending == 0 && p.Mispredictions == 0, "all confirmed, no misprediction");

        // Server disagrees (we were pushed back to 1 at tick 4): later inputs replay on top of it.
        int correctedFrom = -1, correctedTo = -1;
        p.OnCorrected += (was, now) => { correctedFrom = was; correctedTo = now; };
        p.Record(4, In(2)); p.Record(5, In(2)); p.Record(6, In(2)); // predicts 8, 10(wall), 10
        Assert.That(p.Reconcile(4, 1) && p.Predicted == 5 && p.Pending == 2, "replayed 2 pending inputs on the server state (1 + 2 + 2)");
        Assert.That(p.Mispredictions == 1 && correctedFrom == 8 && correctedTo == 1, "correction reported with predicted vs authoritative");

        // Stale / reordered state is ignored.
        Assert.That(!p.Reconcile(3, 99) && p.Predicted == 5, "stale tick ignored");

        // Custom divergence threshold: small drift tolerated.
        var q = new Predictor<int>(0, Step, (a, b) => Math.Abs(a - b) > 1);
        q.Record(1, In(3)); q.Record(2, In(3));
        Assert.That(!q.Reconcile(1, 2) && q.Predicted == 6, "within threshold: no replay");
        Assert.That(q.Reconcile(2, 0) && q.Predicted == 0, "beyond threshold: replay");

        // Capacity: oldest in-flight inputs are dropped.
        var r = new Predictor<int>(0, Step, null, 4);
        for (uint t = 1; t <= 10; t++) r.Record(t, In(1));
        Assert.That(r.Pending == 4 && r.Predicted == 10, "ring keeps the last 4 inputs");
        r.Reset(0); Assert.That(r.Pending == 0 && r.Predicted == 0, "reset");
    }

    static string Hex(byte[] b) { var sb = new System.Text.StringBuilder(); foreach (var x in b) sb.Append(x.ToString("x2")); return sb.ToString(); }

    // Golden vectors shared with the Web SDK tests (sdk/web/test/sync.test.mjs): the same schema + values
    // must encode to the same bytes in C# and TypeScript.
    [Test]
    public void CodecVectors()
    {
        var schema = Schema.Parse("component Transform {\n  pos: vec3 @range(-4096, 4096, 0.01)\n  rot: quat\n}\ncomponent Stats {\n  hp: u16\n  name: string(16)\n  speed: f32\n  team: i8\n  flags: bits(5)\n  seed: u64\n}\n");
        var tr = schema.ByName("Transform"); var st = schema.ByName("Stats");
        var world = new SyncWorld(schema);
        var pos = new ComponentValue { Fields = new[] { new Value { X = 1.25f, Y = -2.5f, Z = 300.75f }, new Value { X = 0.1f, Y = 0.2f, Z = 0.3f, W = 0.9f } } };
        var stats = new ComponentValue { Fields = new[] { new Value { U = 4242 }, new Value { S = "zé" }, new Value { F = 3.5f }, new Value { I = -3 }, new Value { U = 21 }, new Value { U = 0x0123456789abcdefUL } } };
        Assert.That(Hex(world.EncodeWrites(new List<(ComponentDef, ComponentValue)> { (tr, pos), (st, stats) })), Is.EqualTo("023203eb1f8335abdf255296f442486f5875280c000000b50123456789abcdef"), "writes");
        var w = new BitWriter(); w.WriteBits(0x1ff, 9); w.WriteInt(-77, 12); w.WriteQuantized(12.345f, -100, 100, 0.001f); w.WriteF32(1.5f); w.WriteBits(ulong.MaxValue, 64); w.WriteBool(true);
        Assert.That(Hex(w.Finish()), Is.EqualTo("ff84cb6db27f800001ffffffffffffffff"), "bits");
        w = new BitWriter(); foreach (long value in new long[] { 200, -200, 127, -128 }) w.WriteInt(value, 8);
        w.WriteUInt(300, 8); w.WriteInt(long.MaxValue, 64); w.WriteInt(long.MinValue, 64);
        Assert.That(Hex(w.Finish()), Is.EqualTo("fefffefffffffffffffffffffeffffffffffffffff"), "an integer out of range is the nearest end of it");
        Assert.That(Hex(SyncWorld.EncodeInput(0x01020304, new List<byte[]> { new byte[] { 1, 2 }, new byte[0], new byte[] { 9 } })), Is.EqualTo("100102030403020102000109"), "input");
        Assert.That(Hex(SyncWorld.EncodeAck(0xdeadbeef, 0x80000001)), Is.EqualTo("13deadbeef80000001"), "ack");
        Assert.That(SyncWorld.EncodeInput(1, new List<byte[]> { new byte[SyncWorld.MaxInput] }).Length, Is.EqualTo(6 + 1 + SyncWorld.MaxInput), "the longest input");
        Assert.Throws<ArgumentException>(() => SyncWorld.EncodeInput(1, new List<byte[]> { new byte[SyncWorld.MaxInput + 1] }), "an input too long for its length byte is refused, not cut");
    }

    [Test]
    public void TaggedSpawns()
    {
        var world = new SyncWorld(Schema.Parse("component T {\n  x: f32\n}\n"));
        Assert.That(Hex(world.EncodeOps(new[] { SyncOp.SpawnTaggedEntity(0x01020304) })), Is.EqualTo("ff010701020304" + "00"), "op");

        byte[] Answer(uint tag, byte status, ulong entity, uint tick)
        {
            var frame = new List<byte> { 0x14 };
            frame.AddRange(BitConverter.GetBytes(tag).Reverse()); frame.Add(status);
            frame.AddRange(BitConverter.GetBytes(entity).Reverse()); frame.AddRange(BitConverter.GetBytes(tick).Reverse());
            return frame.ToArray();
        }
        var created = SyncWorld.ReadSpawnAnswer(Answer(5, 0, 0x0102030405060708, 77)).Value;
        Assert.That((created.Tag, created.Status, created.Entity, created.Tick), Is.EqualTo((5u, SpawnStatus.Created, 0x0102030405060708ul, 77u)));
        Assert.That(SyncWorld.ReadSpawnAnswer(Answer(6, 1, 0, 78)).Value.Status, Is.EqualTo(SpawnStatus.Refused));
        Assert.That(SyncWorld.ReadSpawnAnswer(Answer(6, 2, 0, 78)).Value.Status, Is.EqualTo(SpawnStatus.Invalid));
        Assert.That(SyncWorld.ReadSpawnAnswer(Answer(5, 0, 1, 1).Take(17).ToArray()), Is.Null, "short");
        Assert.That(SyncWorld.ReadSpawnAnswer(SyncWorld.EncodeAck(1, 1)), Is.Null, "another frame");

        var spawns = new PendingSpawns();
        Assert.That(new[] { spawns.NextTag(5), spawns.NextTag(5), spawns.NextTag(9) }, Is.EqualTo(new uint[] { 5, 6, 9 }));
        spawns.Sent(5, new byte[] { 5 }, 12);
        spawns.Sent(6, new byte[] { 6 }, 13);
        Assert.That(spawns.Take(created), Is.True);
        Assert.That(spawns.Take(created), Is.False, "the answer to a repeat is reported again");
        Assert.That(spawns.Take(new SpawnAnswer { Tag = 0, Entity = 9 }), Is.True, "the avatar");

        // tag 6 went out for tick 13: the room is half a second past it at tick 44
        var repeated = new List<byte[]>();
        uint Send(byte[] body) { repeated.Add(body); return 46; }
        spawns.Repeat(43, 30, Send);
        Assert.That(repeated, Is.Empty);
        spawns.Repeat(44, 30, Send);
        spawns.Repeat(45, 30, Send);
        Assert.That(repeated, Is.EqualTo(new[] { new byte[] { 6 } }));
        Assert.That(spawns.Take(new SpawnAnswer { Tag = 6, Status = SpawnStatus.Refused }), Is.True);
        spawns.Repeat(200, 30, Send);
        Assert.That(repeated.Count, Is.EqualTo(1));
    }

    [Test]
    public void TheLastInputFrameIsSentOnTheNextTwoTicks()
    {
        var inputs = new InputFrames();
        var one = new byte[] { 1 };
        var two = new byte[] { 2 };
        Assert.That(Hex(inputs.Frame(12, one, 10)), Is.EqualTo(Hex(SyncWorld.EncodeInput(12, new[] { one }))));
        var frame = inputs.Frame(13, two, 10);
        Assert.That(Hex(frame), Is.EqualTo(Hex(SyncWorld.EncodeInput(12, new[] { one, two }))));
        Assert.That(inputs.LastTick, Is.EqualTo(13));

        Assert.That(inputs.Tail(10), Is.Null, "repeated within the tick it was sent on");
        Assert.That(Hex(inputs.Tail(11)), Is.EqualTo(Hex(frame)));
        Assert.That(inputs.Tail(11), Is.Null, "repeated twice in one tick");
        Assert.That(Hex(inputs.Tail(12)), Is.EqualTo(Hex(frame)));
        Assert.That(inputs.Tail(13), Is.Null, "repeated a third time");

        // tick 16 does not follow 13, so nothing is carried over
        Assert.That(Hex(inputs.Frame(16, one, 14)), Is.EqualTo(Hex(SyncWorld.EncodeInput(16, new[] { one }))));
        Assert.That(new InputFrames().Tail(5), Is.Null, "nothing sent yet");
    }

    [Test]
    public void TheRenderTickWaitsAtTheRoomsTickForLateDeltas()
    {
        var clock = new RenderClock(2);
        clock.Delta(10);
        clock.Advance(1);
        for (int frame = 0; frame < 3; frame++)
        {
            clock.Advance(1);
            Assert.That(clock.Tick, Is.LessThanOrEqualTo(10d), "frame " + frame);
        }

        for (uint tick = 11; tick <= 14; tick++) clock.Delta(tick);
        Assert.That(clock.Tick, Is.EqualTo(12d));
    }

    [Test]
    public void TheRenderTickIsBackBehindARoomThatStoodStill()
    {
        var clock = new RenderClock(2);
        clock.Delta(10);
        for (int frame = 0; frame < 7; frame++) clock.Advance(1);
        Assert.That(clock.Tick, Is.EqualTo(10d));
        for (uint tick = 11; tick <= 20; tick++)
        {
            clock.Delta(tick);
            Assert.That(clock.Tick, Is.EqualTo(tick - 2d), "tick " + tick);
            clock.Advance(1);
            Assert.That(clock.Tick, Is.LessThanOrEqualTo((double)tick));
        }
    }

    [Test]
    public void CommandATickCannotCarryIsRefused()
    {
        var sent = new List<(Channel, byte[])>();
        var ls = new LockstepClient((ch, b) => sent.Add((ch, b)));
        ls.SendCommand(new byte[LockstepClient.MaxCommand]);
        Assert.That(sent.Count == 1 && sent[0].Item1 == Channel.Reliable && sent[0].Item2.Length == 1 + LockstepClient.MaxCommand, "the longest command leaves whole");
        Assert.Throws<ArgumentException>(() => ls.SendCommand(new byte[LockstepClient.MaxCommand + 1]));
        Assert.Throws<ArgumentException>(() => ls.SendCommand(new byte[0]));
        Assert.That(sent.Count == 1, "a refused command sends nothing");
    }

    [Test]
    public void LockstepTests()
    {
        var sent = new List<(Channel, byte[])>();
        var ls = new LockstepClient((ch, b) => sent.Add((ch, b)));
        // adaptive delay: late inputs raise it at once, steady slack lowers it slowly
        ls.OnInputStats(30, -2, 2);
        Assert.That(ls.InputDelay == 5 && ls.LateInputs == 2, "late by 2 -> delay 2 -> 5");
        ls.OnInputStats(60, 3, 0); ls.OnInputStats(90, 3, 0);
        Assert.That(ls.InputDelay == 5, "two slack reports: unchanged");
        ls.OnInputStats(120, 3, 0);
        Assert.That(ls.InputDelay == 4, "third slack report: -1");
        ls.OnInputStats(150, 1, 0); ls.OnInputStats(180, 3, 0); ls.OnInputStats(210, 3, 0); ls.OnInputStats(240, 3, 0);
        Assert.That(ls.InputDelay == 3, "streak resets on a tight report");
        ls.OnInputStats(270, -12, 30);
        Assert.That(ls.InputDelay == 16, "a round trip of 12 ticks is covered by one report");
        ls.OnInputStats(360, 9, 0); ls.OnInputStats(390, 6, 0); ls.OnInputStats(420, 7, 0);
        Assert.That(ls.InputDelay == 12, "three slack reports give the excess back");
        ls.InputDelay = ls.InputDelayMax; ls.OnInputStats(450, -9, 1);
        Assert.That(ls.InputDelay == ls.InputDelayMax, "capped at max");
        // the same frame through Receive
        Assert.That(ls.Receive(new byte[] { 0x25, 0, 0, 0, 10, unchecked((byte)(sbyte)-1), 1 }) && ls.LastMargin == -1, "0x25 parsed");
        // dump request -> event -> SendDump frame on the reliable channel
        uint asked = 0; ls.OnDumpRequested += t => asked = t;
        ls.Receive(new byte[] { 0x26, 0, 0, 1, 0 });
        Assert.That(asked == 256, "dump requested for tick 256");
        ls.SendDump(256, 0x1122334455667788UL, new byte[] { 7, 7 });
        var d = sent[sent.Count - 1];
        Assert.That(d.Item1 == Channel.Reliable && Hex(d.Item2) == "27000001001122334455667788" + "0707", "dump frame layout");
        // input redundancy window
        ls.InputRedundancy = 2; long stamped = ls.SendInput(new byte[] { 0 });
        Assert.That(sent.Count == 1 && stamped == -1, "no input before the room's tick is known");
        ls.Receive(Catchup(9)); ls.Receive(new byte[] { 0x20, 1, 0, 6, 0x20, 0, 0, 0, 9, 0 });
        long first = ls.SendInput(new byte[] { 1 }), second = ls.SendInput(new byte[] { 2 });
        Assert.That(sent[sent.Count - 1].Item2[5] == 2, "two inputs per datagram");
        Assert.That(first == ls.ServerTick + ls.InputDelay && second == first + 1, $"an input returns the tick it is stamped with: {first}, {second}");
        int frames = sent.Count; stamped = ls.SendInput(new byte[] { 3 });
        Assert.That(sent.Count == frames && stamped == -1, "one input per tick");
        ls.Receive(new byte[] { 0x20, 1, 0, 6, 0x20, 0, 0, 0, 40, 0 });
        var ack = sent[sent.Count - 1];
        Assert.That(ack.Item1 == Channel.Unreliable && Hex(ack.Item2) == "290000000a", "a bundle frame is acknowledged with the tick needed next: 10 while 10 to 39 are missing");
        frames = sent.Count; ls.SendInput(new byte[] { 4 });
        Assert.That(
            sent.Count == frames + 1 && sent[sent.Count - 1].Item2[5] == 1,
            "an input of an earlier, non-adjacent tick is not carried over"
        );
        // the room resends from the acknowledged tick, more than three bundles in one frame
        var resent = new List<byte> { 0x20, 30 };
        for (uint tick = 10; tick < 40; tick++) resent.AddRange(new byte[] { 0, 6, 0x20, (byte)(tick >> 24), (byte)(tick >> 16), (byte)(tick >> 8), (byte)tick, 0 });
        ls.Receive(resent.ToArray());
        Assert.That(ls.ConfirmedTick == 41 && Hex(sent[sent.Count - 1].Item2) == "2900000029", "30 bundles in one frame close the gap; the ack moves to 41");
        ls.InputDelay = 4; ls.OnInputStats(50, -12, 30);
        Assert.That(ls.InputDelay == 17, "late by 12 -> delay 4 -> 17");
        ls.OnInputStats(80, -12, 30);
        Assert.That(ls.InputDelay == 17, "a report that still covers inputs stamped before the raise does not raise again");

        // a room past tick 0: its bundles come before the reliable catch-up
        var acks = new List<string>(); var ticks = new List<uint>(); int restores = 0;
        var late = new LockstepClient((channel, bytes) => acks.Add(Hex(bytes)));
        late.OnTick += (tick, inputs) => ticks.Add(tick); late.OnRestore += (tick, hash, state) => restores++;
        late.Receive(new byte[] { 0x20, 1, 0, 6, 0x20, 0, 0, 0, 5, 0 });
        Assert.That(ticks.Count == 0 && acks[acks.Count - 1] == "2900000000", "ticks do not start from the middle; the ack still asks for tick 0");
        late.Receive(Catchup(5)); late.Receive(new byte[] { 0x20, 1, 0, 6, 0x20, 0, 0, 0, 5, 0 });
        Assert.That(restores == 1 && ticks.Count == 1 && ticks[0] == 5 && acks[acks.Count - 1] == "2900000006", "they start at the catch-up");
        late.Receive(Catchup(3));
        Assert.That(restores == 1 && late.ConfirmedTick == 6, "a catch-up behind the delivered ticks is ignored");
        var restore = Catchup(3); restore[0] = 0x2A;
        late.Receive(restore); late.Receive(new byte[] { 0x20, 1, 0, 6, 0x20, 0, 0, 0, 3, 0 });
        Assert.That(restores == 2 && ticks[ticks.Count - 1] == 3 && acks[acks.Count - 1] == "2900000004", "a restore behind the delivered ticks rewinds");
    }

    static byte[] Catchup(uint tick) => new byte[] { 0x24, (byte)(tick >> 24), (byte)(tick >> 16), (byte)(tick >> 8), (byte)tick, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 };


    sealed class Toy : ISimulation<(ulong state, int steps)>
    {
        public ulong State; public int Steps;
        public void Step(uint tick, Dictionary<ushort, byte[]> inputs, IReadOnlyList<LockstepCommand> commands)
        {
            foreach (var c in commands) { ulong b = c.Data.Length > 0 ? c.Data[0] : 0UL; unchecked { State = State * 131 + c.Actor * 1000UL + b; } }
            foreach (var kv in inputs.OrderBy(k => k.Key)) { ulong b = kv.Value.Length > 0 ? kv.Value[0] : 0UL; unchecked { State = State * 31 + tick * 1000UL + kv.Key * 10UL + b; } }
            unchecked { State = State * 7 + tick; } Steps++;
        }
        public (ulong state, int steps) Snapshot() => (State, Steps);
        public void Restore((ulong state, int steps) s) { State = s.state; Steps = s.steps; }
    }
    static Dictionary<ushort, byte[]> Inputs(params (ushort, byte)[] v) { var d = new Dictionary<ushort, byte[]>(); foreach (var (a, b) in v) d[a] = new[] { b }; return d; }
    static ulong Reference(List<Dictionary<ushort, byte[]>> script) { var s = new Toy(); for (int t = 0; t < script.Count; t++) s.Step((uint)t, script[t], NoCommands); return s.State; }
    static readonly LockstepCommand[] NoCommands = Array.Empty<LockstepCommand>();

    [Test]
    public void RollbackTests()
    {
        var sim = new Toy(); var rb = new LockstepRollback<(ulong, int)>(sim, 1, 0, 8);
        var script = new List<Dictionary<ushort, byte[]>>(); for (int i = 0; i < 10; i++) script.Add(Inputs((1, 5), (2, 9)));
        rb.Confirm(0, script[0], NoCommands); bool rolled = false;
        for (uint t = 1; t < 10; t++) { rb.LocalInput(t, new byte[] { 5 }); rb.Predict(t + 3); rolled |= rb.Confirm(t, script[(int)t], NoCommands); }
        Assert.That(!rolled && rb.Rollbacks == 0 && rb.ConfirmedTick == 10 && rb.HeadTick > rb.ConfirmedTick, "rollback: correct predictions never roll back");

        sim = new Toy(); rb = new LockstepRollback<(ulong, int)>(sim, 1, 0, 8);
        script.Clear(); for (int i = 0; i < 8; i++) script.Add(Inputs((1, 1), (2, 2)));
        script[4] = Inputs((1, 1), (2, 7)); script[5] = Inputs((2, 7));
        for (uint t = 0; t < 8; t++) { rb.LocalInput(t, new byte[] { 1 }); rb.Predict(t + 4); rb.Confirm(t, script[(int)t], NoCommands); }
        Assert.That(rb.Rollbacks == 4 && rb.ResimulatedTicks > 0 && rb.HeadTick == 11, "rollback: mispredictions roll back and re-predict (" + rb.Rollbacks + ")");
        var pure = new Toy(); var rb2 = new LockstepRollback<(ulong, int)>(pure, 1, 0, 8);
        for (uint t = 0; t < 8; t++) rb2.Confirm(t, script[(int)t], NoCommands);
        Assert.That(pure.State == Reference(script), "rollback: confirmed-only run equals pure lockstep");

        sim = new Toy(); rb = new LockstepRollback<(ulong, int)>(sim, 1, 0, 3);
        rb.Predict(100);
        Assert.That(rb.HeadTick == 3 && rb.PredictedTicks == 3 && !rb.Confirm(7, Inputs(), NoCommands), "rollback: prediction bounded, out-of-order confirm ignored");
        rb.Reset(50); rb.Predict(52);
        Assert.That(rb.ConfirmedTick == 50 && rb.HeadTick == 52, "rollback: reset forgets history");

        var ring = new ReplayRing<uint>(50, 10);
        for (uint t = 0; t < 100; t++) ring.Record(t, Inputs((1, (byte)t)), NoCommands, () => t * 100);
        var w = ring.Window(73).Value;
        Assert.That(w.Start == 70 && w.Snapshot == 7000 && w.Inputs.Count == 30 && w.Inputs[0][1][0] == 70, "replay ring: window from nearest snapshot");
        w = ring.Window(5).Value;
        Assert.That(w.Start == 50 && w.Inputs.Count == 50 && ring.Oldest == 50, "replay ring: clamps to the oldest kept snapshot");
        ring.Record(200, Inputs(), NoCommands, () => 1);
        Assert.That(ring.Oldest == 200 && ring.Window(0).Value.Inputs.Count == 1, "replay ring: gap resets");
        ring = new ReplayRing<uint>(50, 10);
        for (uint t = 0; t < 105; t++) ring.Record(t, Inputs((1, (byte)t)), NoCommands, () => t * 100);
        foreach (uint from in new uint[] { 0, 52, 55, 60, 104 })
        {
            w = ring.Window(from).Value;
            Assert.That(w.Snapshot == w.Start * 100 && w.Inputs.Count == 105 - w.Start && w.Inputs[0][1][0] == w.Start, "replay ring: a window is stepped with the inputs of its own ticks");
        }
        Assert.That(ring.Oldest <= 55 && ring.Window(ring.Oldest.Value).Value.Start == ring.Oldest, "replay ring: the oldest tick can be replayed");

    }

    sealed class Doubling : ISimulation<int>
    {
        public int X = 1;
        public void Step(uint tick, Dictionary<ushort, byte[]> inputs, IReadOnlyList<LockstepCommand> commands)
        {
            foreach (var c in commands) X += c.Data[0];
            X = X * 2 + inputs.Values.Sum(input => input[0]);
        }
        public int Snapshot() => X;
        public void Restore(int snapshot) { X = snapshot; }
    }

    // The game runs three ticks ahead; tick 3 carries a command that adds 100 before its step.
    static (int reference, int confirmed, int replayed, ReplayWindow<int> window) CommandAtTick3(byte[] theirs)
    {
        var reference = new Doubling(); var sim = new Doubling();
        var rb = new LockstepRollback<int>(sim, 1, 0, 8); var ring = new ReplayRing<int>(300, 2);
        for (uint t = 0; t < 6; t++)
        {
            rb.LocalInput(t + 3, new byte[] { 1 }); rb.Predict(t + 3);
            var confirmed = Inputs((1, 1), (2, theirs[t]));
            var commands = t == 3 ? new[] { new LockstepCommand(2, new byte[] { 100 }) } : NoCommands;
            reference.Step(t, confirmed, commands);
            int before = rb.ConfirmedSnapshot;
            rb.Confirm(t, confirmed, commands);
            ring.Record(t, confirmed, commands, () => before);
        }
        var w = ring.Window(2).Value; var cam = new Doubling(); cam.Restore(w.Snapshot);
        for (int i = 0; i < w.Inputs.Count; i++) cam.Step(w.Start + (uint)i, w.Inputs[i], w.Commands[i]);
        return (reference.X, rb.ConfirmedSnapshot, cam.X, w);
    }

    [Test]
    public void RollbackCommandTests()
    {
        var run = CommandAtTick3(new byte[] { 0, 0, 0, 0, 0, 0 });
        Assert.That(run.reference == 927 && run.confirmed == 927 && run.replayed == 927, "rollback: a confirmed tick with a command is stepped again with it (" + run.confirmed + ", replayed " + run.replayed + ")");
        var w = run.window;
        Assert.That(w.Start == 2 && w.Commands.Count == w.Inputs.Count && w.Commands[1].Count == 1 && w.Commands[1][0].Actor == 2 && w.Commands[1][0].Data[0] == 100 && w.Commands.Count(c => c.Count > 0) == 1, "replay ring: keeps the commands of a tick with its inputs");
        run = CommandAtTick3(new byte[] { 0, 0, 0, 0, 7, 7 });
        Assert.That(run.reference == 948 && run.confirmed == 948 && run.replayed == 948, "rollback: a command and a mispredicted input after it give the pure lockstep state (" + run.confirmed + ", replayed " + run.replayed + ")");

        var ls = new LockstepClient((ch, p) => { }); var ticks = new List<string>();
        ls.OnTick += (t, inputs) => ticks.Add(t + ":" + string.Join(",", inputs.Keys) + ":" + string.Join(",", ls.Commands.Select(c => c.Actor + "=" + Hex(c.Data))));
        ls.Receive(new byte[] { 0x20, 1, 0, 24, 0x20, 0, 0, 0, 0, 2, 0, 1, 1, 5, 0xFF, 0xFF, 11, 0, 2, 0, 1, 9, 0, 1, 0, 2, 7, 8 });
        ls.Receive(new byte[] { 0x20, 1, 0, 10, 0x20, 0, 0, 0, 1, 1, 0, 1, 1, 5 });
        Assert.That(string.Join(" ", ticks) == "0:1:2=09,1=0708 1:1:" && ls.Commands.Count == 0, "lockstep: OnTick has the commands of its tick apart from the players' inputs (" + string.Join(" ", ticks) + ")");
    }


    sealed class Scripted : IBrain
    {
        public uint T; public uint PatrolLeft;
        public bool Condition(uint id, Blackboard bb) => id == 1 ? (T >= 8 && T <= 12) || (T >= 20 && T <= 22) : id == 2 && bb.Get(50) == 1;
        public Status Action(uint id, Blackboard bb)
        {
            switch (id)
            {
                case 10: if (PatrolLeft == 0) { PatrolLeft = 3; return Status.Success; } PatrolLeft--; return Status.Running;
                case 11: bb.Set(100, bb.Get(100) + 1); return Status.Running;
                case 12: bb.Set(101, bb.Get(101) + 1); return Status.Success;
                case 20: return Status.Success;
                case 30: return Status.Running;
                default: if (id >= 100 && id <= 103) { bb.Set(id, bb.Get(id) + 1); return Status.Success; } return Status.Failure;
            }
        }
        public void Abort(uint id, Blackboard bb) => bb.Set(102, bb.Get(102) + 1);
    }

    static string BbJson(Blackboard bb) => "{" + string.Join(",", bb.Values.Select(kv => "\"" + kv.Key + "\":" + kv.Value)) + "}";
    static string BbJson(JsonElement e) => "{" + string.Join(",", e.EnumerateObject().OrderBy(p => uint.Parse(p.Name)).Select(p => "\"" + p.Name + "\":" + p.Value.GetInt64())) + "}";

    [Test]
    public void BotsTests()
    {
        var dir = AppContext.BaseDirectory;
        while (dir != null && !Directory.Exists(Path.Combine(dir, "crates"))) dir = Path.GetDirectoryName(dir.TrimEnd(Path.DirectorySeparatorChar));
        var doc = JsonDocument.Parse(File.ReadAllText(Path.Combine(dir, "crates/net-sim-bots/fixtures/vectors.json"))).RootElement;
        var tree = new BehaviorTree(BtNode.Parse(doc.GetProperty("tree").GetRawText()));
        var bb = new Blackboard(); var brain = new Scripted { PatrolLeft = 2 };
        bool ok = true; uint t = 0;
        foreach (var exp in doc.GetProperty("tree_ticks").EnumerateArray())
        {
            brain.T = t; var st = tree.Step(brain, bb);
            ok &= st.ToString() == exp.GetProperty("status").GetString() && "0x" + tree.Hash().ToString("x") == exp.GetProperty("hash").GetString() && BbJson(bb) == BbJson(exp.GetProperty("bb"));
            t++;
        }
        Assert.That(ok, "bots: behavior tree replays the Rust vectors (" + t + " ticks)");
        var fsm = StateMachine.FromJson(doc.GetProperty("fsm").GetRawText());
        var fbb = new Blackboard(); var fbrain = new Scripted();
        fsm.Start(fbrain, fbb);
        bool fok = true; t = 0;
        foreach (var exp in doc.GetProperty("fsm_ticks").EnumerateArray())
        {
            fbrain.T = t; if (t == 25) fbb.Set(50, 1);
            fsm.Step(fbrain, fbb);
            fok &= fsm.Current == exp.GetProperty("current").GetUInt32() && "0x" + fsm.Hash().ToString("x") == exp.GetProperty("hash").GetString() && BbJson(fbb) == BbJson(exp.GetProperty("bb"));
            t++;
        }
        Assert.That(fok, "bots: state machine replays the Rust vectors (" + t + " ticks)");
    }

    [Test]
    public void SimTests()
    {
        try { SimChecks(); }
        catch (DllNotFoundException) { Assert.Ignore("libnet_sim_wasm not built (cargo build --release -p net-sim-wasm)"); }
    }

    static void SimChecks()
    {
        Assert.That(SimWorld.GoldenHash() == 0x58d9870ba45952b5UL, "sim: golden hash equals the Rust/wasm value");
        using var a = new SimWorld();
        using var b = new SimWorld();
        uint ballA = 0, ballB = 0;
        foreach (var (w, isA) in new[] { (a, true), (b, false) })
        {
            w.AddBox(BodyKind.Static, 0, -1, 50, 1);
            uint ball = w.AddCircle(BodyKind.Dynamic, 0, 5, 1);
            w.SetMaterial(ball, 0.3, 0.2);
            if (isA) ballA = ball; else ballB = ball;
        }
        for (int i = 0; i < 120; i++) { a.Step(); b.Step(); }
        Assert.That(a.Hash() == b.Hash() && ballA == ballB, "sim: two worlds agree");
        double y = a.Body(ballA).Y;
        Assert.That(y > 0.9 && y < 1.1, "sim: ball rests on the floor (" + y.ToString("F3") + ")");
        var snap = a.Snapshot(); ulong h = a.Hash(); a.Step();
        Assert.That(a.Restore(snap) && a.Hash() == h, "sim: snapshot round-trip");
        Assert.That(a.Raycast(0, 5, 0, -10, out var hit) && hit.Body == ballA && Math.Abs(hit.Ny - 1) < 1e-6, "sim: raycast hits the ball from above");
        Assert.That(SimWorld.NavGoldenHash() == 0x746c5a608abe2f26UL, "nav: golden hash equals the Rust/wasm value");
        Assert.That(SimWorld.JointGoldenHash() == 0x4bcc781a484ceeedUL, "joints: golden hash equals the Rust/wasm value");
        Assert.That(Sim3World.GoldenHash() == 0x0a9c7e6fc4fa43bfUL, "sim3: golden hash equals the Rust/wasm value");
        using (var w3 = new Sim3World())
        {
            w3.AddBox(BodyKind.Static, new Vec3(0, -0.5, 0), new Vec3(10, 0.5, 10));
            uint ball3 = w3.AddSphere(BodyKind.Dynamic, new Vec3(0, 3, 0), 0.5);
            for (int i = 0; i < 240; i++) w3.Step();
            Assert.That(Math.Abs(w3.Body(ball3).Position.Y - 0.5) < 0.03, "sim3: sphere rests on the floor");
            var snap3 = w3.Snapshot(); ulong h3 = w3.Hash();
            w3.SetPosition(ball3, new Vec3(0, 9, 0));
            Assert.That(w3.Hash() != h3 && w3.Restore(snap3) && w3.Hash() == h3, "sim3: snapshot round-trip");
            Assert.That(w3.Raycast(new Vec3(0, 5, 0), new Vec3(0, -10, 0), out var h3hit) && h3hit.Body == ball3 && Math.Abs(h3hit.Point.Y - 1) < 0.05, "sim3: raycast hits the sphere");
        }
        Assert.That(Sim3World.JointGoldenHash() == 0x5db56af63cceee85UL, "sim3: joint golden hash equals the Rust/wasm value");
        Assert.That(Sim3World.HullGoldenHash() == 0xd656fc8f55999cf8UL, "sim3: hull golden hash equals the Rust/wasm value");
        using (var c3 = new Sim3World())
        {
            c3.AddBox(BodyKind.Static, new Vec3(0, -0.5, 0), new Vec3(20, 0.5, 20));
            var verts = new[] { new Vec3(-0.5, -0.25, -0.5), new Vec3(0.5, -0.25, -0.5), new Vec3(0.5, -0.25, 0.5), new Vec3(-0.5, -0.25, 0.5), new Vec3(0, 0.75, 0) };
            var faces = new[] { new uint[] { 0, 1, 2, 3 }, new uint[] { 0, 1, 4 }, new uint[] { 1, 2, 4 }, new uint[] { 2, 3, 4 }, new uint[] { 3, 0, 4 } };
            uint pyr = c3.AddConvex(BodyKind.Dynamic, new Vec3(0, 2, 0), verts, faces);
            for (int i = 0; i < 300; i++) c3.Step();
            var pp = c3.Body(pyr).Position;
            Assert.That(Math.Abs(pp.Y - 0.25) < 0.06, "sim3: convex pyramid rests on the floor (" + pp.Y.ToString("F3") + ")");
            Assert.That(c3.SphereCast(new Vec3(0, 5, 0), new Vec3(0, -10, 0), 0.5, out var sh) && sh.Body == pyr && Math.Abs(sh.Point.Y - 1.0) < 0.05 && sh.Normal.Y > 0.5, "sim3: sphere cast hits the pyramid apex");
            var q0 = new Quat(0, 0, 0, 1);
            Assert.That(c3.BoxCast(new Vec3(0, 5, 0), q0, new Vec3(0.5, 0.5, 0.5), new Vec3(0, -10, 0), out var bh) && bh.Body == pyr && Math.Abs(bh.Point.Y - 1.0) < 0.05 && Math.Abs(bh.Fraction - 0.35) < 0.01, "sim3: box cast stops on the pyramid apex (y=" + bh.Point.Y.ToString("F3") + ")");
            Assert.That(c3.CapsuleCast(new Vec3(0, 5, 0), q0, 0.5, 0.25, new Vec3(0, -10, 0), out var ch) && ch.Body == pyr && Math.Abs(ch.Point.Y - 1.0) < 0.05 && Math.Abs(ch.Fraction - 0.325) < 0.01, "sim3: capsule cast stops on the pyramid apex (y=" + ch.Point.Y.ToString("F3") + ")");
            Assert.That(!c3.BoxCast(new Vec3(0, 5, 0), q0, new Vec3(0.5, 0.5, 0.5), new Vec3(10, 0, 0), out _), "sim3: box cast misses sideways");
        }
        using (var j3 = new Sim3World())
        {
            j3.AddBox(BodyKind.Static, new Vec3(0, -0.5, 0), new Vec3(20, 0.5, 20));
            uint post = j3.AddBox(BodyKind.Static, new Vec3(-10, 1, 0), new Vec3(0.1, 1, 0.1));
            uint door = j3.AddBox(BodyKind.Dynamic, new Vec3(-9.4, 1, 0), new Vec3(0.5, 1, 0.05));
            uint hinge = j3.JointHinge(post, door, new Vec3(0.1, 0, 0), new Vec3(-0.5, 0, 0), new Vec3(0, 1, 0), new Vec3(0, 1, 0), 0, Math.PI / 2);
            j3.ApplyImpulse(door, new Vec3(0, 0, 8), new Vec3(-9, 1, 0));
            j3.AddBox(BodyKind.Static, new Vec3(4, 0.15, 10), new Vec3(1, 0.15, 2));
            uint hero = j3.AddCapsule(BodyKind.Kinematic, new Vec3(0, 0.8, 10), 0.5, 0.3);
            uint k = j3.Character(hero);
            for (int i = 0; i < 300; i++) { j3.MoveCharacter(k, new Vec3(3.0 / 60, -10.0 / 60, 0)); j3.Step(); }
            double angle = j3.HingeAngle(hinge);
            Assert.That(angle > 0.3 && angle < Math.PI / 2 + 0.1, "sim3: hinged door opened within its limits (" + angle.ToString("F2") + ")");
            var hp = j3.Body(hero).Position;
            Assert.That(hp.X > 5 && Math.Abs(hp.Y - 0.8) < 0.05 && j3.CharacterState(k).Grounded, "sim3: character stepped over the ledge and stands (" + hp.X.ToString("F2") + "," + hp.Y.ToString("F2") + ")");
        }
        using (var j = new SimWorld(0, -10))
        {
            uint floor = j.AddBox(BodyKind.Static, 0, -0.5, 10, 0.5);
            uint ball = j.AddCircle(BodyKind.Dynamic, 0, 2, 0.5);
            int begins = 0;
            for (int i = 0; i < 120; i++) { j.Step(); foreach (var e in j.Events()) if (e.Begin && ((e.A == ball && e.B == floor) || (e.A == floor && e.B == ball))) begins++; }
            Assert.That(begins >= 1, "joints: ball/floor contact begin reported");
            Assert.That(j.CircleCast(-5, 0.5, 10, 0, 0.4, out var ch) && ch.Body == ball && Math.Abs(ch.X + 0.9) < 0.01, "joints: circle cast stops one radius short of the ball");
        }
        using var n = new SimWorld(0, 0);
        n.NavGrid(0, 0, 1, 8, 6, new (uint, uint)[] { (4, 0), (4, 1), (4, 2), (4, 3), (4, 4) });
        var path = n.NavPath(0.5, 0.5, 7.5, 0.5);
        Assert.That(path != null && Array.Exists(path, p => p.x == 4 && p.y == 5), "nav: path rounds the wall");
        Assert.That(n.NavRaycast(0.5, 0.5, 7.5, 0.5, out _, out _), "nav: wall blocks line of sight");
        uint field = n.NavField(7.5, 0.5);
        uint ag = n.AddAgent(0.5, 0.5, 0.25, 3);
        n.AgentFollow(ag, field);
        for (int i = 0; i < 600; i++) n.CrowdStep();
        var st = n.Agent(ag);
        Assert.That(Math.Sqrt((st.X - 7.5) * (st.X - 7.5) + (st.Y - 0.5) * (st.Y - 0.5)) < 0.5, "nav: agent reached the goal (" + st.X.ToString("F2") + "," + st.Y.ToString("F2") + ")");
    }

    // One datagram per message on `channel`, and what the far side delivers when they arrive in `order`.
    static int[] Delivered(Channel channel, params int[] order)
    {
        var near = new Link(); var far = new Link(); var datagrams = new List<byte[]>();
        for (int i = 0; i < 3; i++) { near.Send(channel, new[] { (byte)i }); near.FlushBatch(); while (near.TryDequeueOutgoing(out var d)) datagrams.Add(d); }
        return order.SelectMany(i => far.Receive(datagrams[i])).Select(m => m.Channel == channel ? (int)m.Payload[0] : -1).ToArray();
    }

    [Test]
    public void TheUnsequencedChannelDeliversWhatArrivesOutOfOrder()
    {
        Assert.That(Delivered(Channel.UnreliableUnsequenced, 2, 0, 1), Is.EqualTo(new[] { 2, 0, 1 }));
        Assert.That(Delivered(Channel.Unreliable, 2, 0, 1), Is.EqualTo(new[] { 2 }));
    }

    [Test]
    public void TheUnsequencedChannelHasADatagramAndACounterOfItsOwn()
    {
        var near = new Link(); var datagrams = new List<byte[]>();
        near.Send(Channel.Unreliable, new byte[] { 1 }); near.Send(Channel.UnreliableUnsequenced, new byte[] { 2 }); near.FlushBatch();
        while (near.TryDequeueOutgoing(out var d)) datagrams.Add(d);
        // version 1, kind | batch flag, seq 0: each channel counts from 0
        Assert.That(datagrams.Select(d => d.Take(4).ToArray()), Is.EqualTo(new[] { new byte[] { 1, 0x20, 0, 0 }, new byte[] { 1, 0x24, 0, 0 } }));
    }

    static List<byte[]> Drain(Link link) { var sent = new List<byte[]>(); while (link.TryDequeueOutgoing(out var datagram)) sent.Add(datagram); return sent; }

    [Test]
    public void ALinkNamesTheReliableMessagesItsPeerHasNotAcknowledged()
    {
        var near = new Link(); var far = new Link();
        (Channel, byte[], int)[] Listed() => near.Unacknowledged().Select(m => (m.Channel, m.Msg, m.Mark)).ToArray();
        byte[] small = { 1 }, loose = { 2 }, large = Enumerable.Repeat((byte)3, 3000).ToArray(), last = { 4 };
        near.Send(Channel.Reliable, small, 0, 7);
        near.Send(Channel.ReliableUnordered, loose, 0, 8);
        near.Send(Channel.Unreliable, new byte[] { 9 }, 0, 9);
        near.Send(Channel.Unreliable, new byte[3000], 0, 9);
        near.Send(Channel.Reliable, large, 0, 0);
        near.Send(Channel.Reliable, last, 0, 7);
        Assert.That(
            Listed(),
            Is.EqualTo(new[] { (Channel.Reliable, small, 7), (Channel.ReliableUnordered, loose, 8), (Channel.Reliable, large, 0), (Channel.Reliable, last, 7) }),
            "unreliable messages are not among them, a message in parts is there once");

        // the second part of the large reliable message is lost: the three parts of the unreliable one come first
        int fragments = 0;
        near.FlushBatch();
        foreach (var datagram in Drain(near))
        {
            if ((datagram[1] & 0x1f) == 3 && (datagram[1] & 0x80) == 0 && fragments++ == 4) continue;
            far.Receive(datagram, 0);
        }
        far.Poll(100);
        foreach (var datagram in Drain(far)) near.Receive(datagram, 100);
        var left = new[] { (Channel.Reliable, large, 0) };
        Assert.That(Listed(), Is.EqualTo(left), "a message is there until every part is acknowledged");

        var queue = new Link();
        var many = Enumerable.Range(0, 40).Select(i => new[] { (byte)i }).ToArray();
        foreach (var msg in many) queue.Send(Channel.Reliable, msg, 0);
        Assert.That(queue.Unacknowledged().Select(m => m.Msg), Is.EqualTo(many), "what waits for the window is among them");

        near.Close();
        Assert.That(Listed(), Is.EqualTo(left), "a link that has ended keeps the list");
    }

    [TestCase(true)]
    [TestCase(false)]
    public void ATimerThatComesLateResendsAPacketBeforeTheLinkIsGivenUp(bool answered)
    {
        var near = new Link(); var far = new Link();
        near.Send(Channel.Reliable, new byte[] { 7 }, 0);
        Assert.That(Drain(near).Count, Is.EqualTo(1), "the packet goes out, and is lost");
        double late = Link.IdleTimeout + 5000;
        Assert.That(near.Poll(late), Is.True, "no resend had its chance yet");
        var again = Drain(near);
        Assert.That(again.Count, Is.EqualTo(1), "the packet is resent");
        if (answered)
        {
            foreach (var datagram in again) far.Receive(datagram, late);
            far.Poll(late + 100);
            foreach (var datagram in Drain(far)) near.Receive(datagram, late + 100);
            Assert.That(near.Poll(late + 2 * Link.IdleTimeout), Is.True, "the resend was acknowledged");
        }
        else
        {
            Assert.That(near.Poll(late + Link.IdleTimeout - 1), Is.True);
            Assert.That(near.Poll(late + Link.IdleTimeout), Is.False, "the resend went unanswered for the idle timeout");
        }
    }

    [Test]
    public void ALinkDiesWhenItsOldestReliablePacketGoesUnacknowledgedForTheIdleTimeout()
    {
        var near = new Link();
        Assert.That(near.Poll(4 * Link.IdleTimeout), Is.True, "nothing waits for the peer");
        double start = 4 * Link.IdleTimeout;
        near.Send(Channel.Reliable, new byte[] { 1 }, start);
        Assert.That(near.Poll(start + Link.IdleTimeout - 1), Is.True);
        Assert.That(near.Poll(start + Link.IdleTimeout), Is.False);
        Assert.That(near.IsDead, Is.True);
    }

    [Test]
    public void ALinkDiesWhenAPacketStaysUnacknowledgedWhileThePeerTalks()
    {
        var near = new Link(); var far = new Link();
        near.Send(Channel.Reliable, new byte[] { 1 }, 0);
        double now = 0;
        for (; ; )
        {
            // the reliable packet never arrives; the peer keeps sending
            while (near.TryDequeueOutgoing(out _)) { }
            far.Send(Channel.Unreliable, new byte[] { 2 }, now);
            far.Poll(now);
            while (far.TryDequeueOutgoing(out var datagram)) near.Receive(datagram, now);
            if (!near.Poll(now)) break;
            now += 100;
        }
        Assert.That(now, Is.EqualTo(Link.IdleTimeout));
        Assert.That(near.IsDead, Is.True);
    }

    [Test]
    public void APacketIsRepeated250MsAfterItLeftWhileTheRoundTripIsUnknown()
    {
        var near = new Link();
        near.Send(Channel.Reliable, new byte[] { 1 }, 0);
        Assert.That(near.TryDequeueOutgoing(out _), Is.True);
        near.Poll(249);
        Assert.That(near.TryDequeueOutgoing(out _), Is.False);
        near.Poll(250);
        Assert.That(near.TryDequeueOutgoing(out _), Is.True);
        Assert.That(near.Retransmits, Is.EqualTo(1));
    }

    [Test]
    public void ASenderThatOutrunsItsPeerLosesTheLink()
    {
        var near = new Link(); var message = new byte[60000]; int accepted = 0;
        var refused = Assert.Throws<InvalidOperationException>(() => { for (; ; ) { near.Send(Channel.Reliable, message, 0); accepted++; } });
        Assert.That(refused.Message, Does.Contain("outran the peer"));
        // the window takes 32 fragments of the first message; sixteen more fill the megabyte
        Assert.That(accepted, Is.EqualTo(17));
        Assert.That(near.IsDead, Is.True);
        Assert.That(near.Backlog(), Is.EqualTo(0));
        Assert.That(near.TryDequeueOutgoing(out _), Is.False);
    }

    [Test]
    public void ADeadLinkRefusesToSendAndKeepsNothing()
    {
        var near = new Link();
        for (int i = 0; i < 100; i++) near.Send(Channel.Reliable, new byte[] { 1 }, 0);
        for (double at = 1000; at < Link.IdleTimeout; at += 1000) Assert.That(near.Poll(at), Is.True);
        Assert.That(near.Poll(Link.IdleTimeout), Is.False);
        Assert.That(near.Backlog(), Is.EqualTo(0));
        Assert.That(near.TryDequeueOutgoing(out _), Is.False);
        foreach (var channel in new[] { Channel.Reliable, Channel.Unreliable })
            Assert.That(Assert.Throws<InvalidOperationException>(() => near.Send(channel, new byte[] { 1 }, Link.IdleTimeout)).Message, Does.Contain("link dead"));
        near.FlushBatch();
        Assert.That(near.TryDequeueOutgoing(out _), Is.False);
        Assert.That(near.Poll(2 * Link.IdleTimeout), Is.False);
    }

    // One packet of a 60 KB message is lost: the retransmit must still be acked and the message arrive.
    [Test]
    public void LinkTests()
    {
        var a = new Link(); var b = new Link();
        var msg = new byte[60000]; for (int i = 0; i < msg.Length; i++) msg[i] = (byte)(i % 251);
        bool lost = false, alive = true; byte[] got = null;
        a.Send(Channel.Reliable, msg);
        var until = DateTime.UtcNow.AddSeconds(8);
        while (got == null && alive && DateTime.UtcNow < until)
        {
            while (a.TryDequeueOutgoing(out var d))
            {
                // lose the first transmission of the first fragment packet (kind 3, not ack-only)
                if (!lost && (d[1] & 0x1f) == 3 && (d[1] & 0x80) == 0) { lost = true; continue; }
                foreach (var r in b.Receive(d)) got = r.Payload;
            }
            while (b.TryDequeueOutgoing(out var d)) a.Receive(d);
            System.Threading.Thread.Sleep(5);
            alive = a.Poll();
        }
        Assert.That(lost && alive && got != null && got.SequenceEqual(msg), "link: a lost packet in a burst longer than the ack window is retransmitted");
    }

    [Test]
    public void ControlJsonTests()
    {
        const string joined = "{\"t\":\"joined\",\"name\":\"actor\",\"mode\":\"relay\",\"actor\":7,"
            + "\"players\":[{\"actor\":3,\"name\":\"schema_hash\"}],\"schema_hash\":\"ab\\\"c\"}";
        Assert.That(Aurix.Net.Managed.ManagedClientTestHooks.Num(joined, "actor") == 7, "json: a room named like a field does not hide the field");
        Assert.That(Aurix.Net.Managed.ManagedClientTestHooks.Str(joined, "schema_hash") == "ab\"c", "json: a player named like a field does not hide the field");
    }

    [Test]
    public void SeedKeepsEveryDigit()
    {
        const string joined = "{\"t\":\"joined\",\"mode\":\"lockstep\",\"tick\":1,\"seed\":1795052993049961715}";
        Assert.That(Aurix.Net.Managed.ManagedClientTestHooks.ULong(joined, "seed"), Is.EqualTo(1795052993049961715UL));
        Assert.That(Aurix.Net.Managed.ManagedClientTestHooks.ULong(joined, "mode"), Is.Null);
    }

    // A node that signs the client in and, on its next message, sends it `count` notices.
    static async Task Flood(HttpListener listener, int count)
    {
        var socket = (await (await listener.GetContextAsync()).AcceptWebSocketAsync(null)).WebSocket;
        var buffer = new byte[4096];
        Task Say(string json) => socket.SendAsync(new ArraySegment<byte>(Encoding.UTF8.GetBytes(json)), WebSocketMessageType.Text, true, CancellationToken.None);
        try
        {
            await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None);
            await Say("{\"t\":\"authed\",\"player_id\":\"ann\",\"name\":\"Ann\"}");
            await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None);
            for (int i = 0; i < count; i++) await Say("{\"t\":\"notice\",\"text\":\"" + i + "\",\"level\":\"info\"}");
            while ((await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None)).MessageType != WebSocketMessageType.Close) { }
        }
        catch (WebSocketException) { }
    }

    [Test]
    public async Task ClientStopsReadingWhenTheGameDoesNotPoll()
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://127.0.0.1:" + port + "/"); listener.Start();
        const int count = ManagedClient.EventBacklog + 2000;
        _ = Flood(listener, count);
        using var client = await ManagedClient.ConnectAsync("http://127.0.0.1:" + port, "grant");
        var notices = new List<string>(); client.OnNotice += (text, level) => notices.Add(text);
        await client.SendControlAsync("{\"t\":\"go\"}");

        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (ManagedClientTestHooks.Waiting(client) < ManagedClient.EventBacklog && clock.ElapsedMilliseconds < 5000) await Task.Delay(10);
        await Task.Delay(300);
        Assert.That(ManagedClientTestHooks.Waiting(client), Is.EqualTo(ManagedClient.EventBacklog));

        clock.Restart();
        while (notices.Count < count && clock.ElapsedMilliseconds < 5000) { client.Poll(); await Task.Delay(1); }
        Assert.That(notices, Is.EqualTo(Enumerable.Range(0, count).Select(i => i.ToString())));
    }

    // A node that signs the client in and, on its next message, gives the data path up.
    static async Task GiveTheDataPathUp(HttpListener listener)
    {
        var socket = (await (await listener.GetContextAsync()).AcceptWebSocketAsync(null)).WebSocket;
        var buffer = new byte[4096];
        Task Say(string json) => socket.SendAsync(new ArraySegment<byte>(Encoding.UTF8.GetBytes(json)), WebSocketMessageType.Text, true, CancellationToken.None);
        // the client sends probes of its own between the messages of the test
        async Task Hear(string type)
        {
            while (!Encoding.UTF8.GetString(buffer, 0, (await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None)).Count).Contains("\"t\":\"" + type + "\"")) { }
        }
        try
        {
            await Hear("auth");
            await Say("{\"t\":\"authed\",\"player_id\":\"ann\",\"name\":\"Ann\"}");
            await Hear("go");
            await Say("{\"t\":\"error\",\"code\":\"transport\",\"message\":\"\"}");
            while ((await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None)).MessageType != WebSocketMessageType.Close) { }
        }
        catch (WebSocketException) { }
    }

    [Test]
    public async Task AHandlerThatThrowsOnTheErrorOfTheDataPathDoesNotHideItsLoss()
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://127.0.0.1:" + port + "/"); listener.Start();
        _ = GiveTheDataPathUp(listener);
        using var client = await ManagedClient.ConnectAsync("http://127.0.0.1:" + port, "grant");
        var closed = new List<string>(); int thrown = 0;
        client.OnControl += json => throw new InvalidOperationException("a bug in the game");
        client.OnClosed += closed.Add;
        await client.SendControlAsync("{\"t\":\"go\"}");

        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (closed.Count == 0 && clock.ElapsedMilliseconds < 5000)
        {
            try { client.Poll(); } catch (InvalidOperationException) { thrown++; }
            await Task.Delay(10);
        }
        Assert.That(thrown, Is.EqualTo(1));
        Assert.That(closed, Is.EqualTo(new[] { "link dead" }));
    }

    // A node that answers a ping only when the next one arrives, as a link slower than the probes would.
    static async Task AnswerLate(HttpListener listener)
    {
        var socket = (await (await listener.GetContextAsync()).AcceptWebSocketAsync(null)).WebSocket;
        var buffer = new byte[4096];
        Task Say(string json) => socket.SendAsync(new ArraySegment<byte>(Encoding.UTF8.GetBytes(json)), WebSocketMessageType.Text, true, CancellationToken.None);
        string held = null;
        try
        {
            while (true)
            {
                var read = await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None);
                if (read.MessageType == WebSocketMessageType.Close) break;
                string json = Encoding.UTF8.GetString(buffer, 0, read.Count);
                if (ManagedClientTestHooks.Str(json, "t") == "auth") await Say("{\"t\":\"authed\",\"player_id\":\"ann\",\"name\":\"Ann\"}");
                if (ManagedClientTestHooks.Str(json, "t") != "ping") continue;
                if (held != null) await Say("{\"t\":\"pong\",\"ts\":" + held + ",\"server_ms\":0}");
                held = ((long)ManagedClientTestHooks.Num(json, "ts")).ToString();
            }
        }
        catch (WebSocketException) { }
    }

    [Test]
    public async Task LateAnswersToProbesStayWithTheClient()
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://127.0.0.1:" + port + "/"); listener.Start();
        _ = AnswerLate(listener);
        using var client = await ManagedClient.ConnectAsync("http://127.0.0.1:" + port, "grant");
        var pongs = new List<double>();
        client.OnControl += json => { if (ManagedClientTestHooks.Str(json, "t") == "pong") pongs.Add(ManagedClientTestHooks.Num(json, "ts")); };
        async Task Run(int ms) { var clock = System.Diagnostics.Stopwatch.StartNew(); while (clock.ElapsedMilliseconds < ms) { client.Poll(); await Task.Delay(10); } }

        await Run(3300);
        await client.SendControlAsync("{\"t\":\"ping\",\"ts\":987654321}");
        await Run(1200);
        Assert.That(pongs, Is.EqualTo(new[] { 987654321d }));
        Assert.That(client.RttMs, Is.GreaterThan(0f));
    }

    // A node that repeats the id of a join: ahead of its refusal of one it sends the `joined` of a join
    // the client gave up. It does not answer a join into the room `silent`, and a join into the room
    // `blacklist` it follows with the `left` of a kick, at once or after the milliseconds in `kick_after`,
    // without ever opening the data path.
    static async Task AnswerAJoinGivenUp(HttpListener listener, List<string> asked)
    {
        var socket = (await (await listener.GetContextAsync()).AcceptWebSocketAsync(null)).WebSocket;
        var buffer = new byte[4096];
        Task Say(string json) => socket.SendAsync(new ArraySegment<byte>(Encoding.UTF8.GetBytes(json)), WebSocketMessageType.Text, true, CancellationToken.None);
        await Say("{\"t\":\"hello\",\"session\":\"s\",\"version\":\"0\",\"region\":\"local\",\"supports\":[\"join_id\"]}");
        try
        {
            while (true)
            {
                var read = await socket.ReceiveAsync(new ArraySegment<byte>(buffer), CancellationToken.None);
                if (read.MessageType == WebSocketMessageType.Close) break;
                string json = Encoding.UTF8.GetString(buffer, 0, read.Count);
                string type = ManagedClientTestHooks.Str(json, "t");
                if (type == "auth") await Say("{\"t\":\"authed\",\"player_id\":\"ann\",\"name\":\"Ann\"}");
                if (type != "join" && type != "leave") continue;
                asked.Add(json);
                ulong id = ManagedClientTestHooks.ULong(json, "id") ?? 0;
                if (type == "leave") { await Say("{\"t\":\"left\",\"reason\":\"left\",\"id\":" + id + "}"); continue; }
                if (ManagedClientTestHooks.Str(json, "name") == "silent") continue;
                if (ManagedClientTestHooks.Str(json, "name") == "blacklist")
                {
                    await Say("{\"t\":\"joined\",\"room\":\"b\",\"name\":\"blacklist\",\"mode\":\"relay\",\"actor\":2,\"master\":1,\"max_players\":4,\"props\":{},\"players\":[],\"data\":{\"host\":\"\",\"port\":0,\"cert_sha256\":\"\",\"ticket\":\"\",\"ws_tunnel\":true},\"cached\":[],\"id\":" + id + "}");
                    await Task.Delay((int)(ManagedClientTestHooks.ULong(json, "kick_after") ?? 0));
                    await Say("{\"t\":\"left\",\"reason\":\"cheater\",\"room\":\"b\"}");
                    continue;
                }
                await Say("{\"t\":\"joined\",\"room\":\"r\",\"name\":\"given-up\",\"mode\":\"relay\",\"actor\":1,\"master\":1,\"max_players\":4,\"props\":{},\"players\":[],\"data\":{\"host\":\"\",\"port\":0,\"cert_sha256\":\"\",\"ticket\":\"\",\"ws_tunnel\":true},\"cached\":[],\"id\":" + (id + 1) + "}");
                await Say("{\"t\":\"error\",\"code\":\"room_closed\",\"message\":\"\",\"id\":" + id + "}");
            }
        }
        catch (WebSocketException) { }
    }

    [Test]
    public async Task AJoinedThatRepeatsTheIdOfAnotherJoinDoesNotAnswerThisOne()
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://127.0.0.1:" + port + "/"); listener.Start();
        var asked = new List<string>();
        _ = AnswerAJoinGivenUp(listener, asked);
        using var client = await ManagedClient.ConnectAsync("http://127.0.0.1:" + port, "grant");

        var refused = Assert.ThrowsAsync<NetException>(() => client.JoinRawAsync("{\"t\":\"join\",\"name\":\"room\"}", 2000));
        Assert.That(refused.Message, Does.StartWith("room_closed"));
        await client.LeaveAsync();
        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (asked.Count < 2 && clock.ElapsedMilliseconds < 2000) await Task.Delay(10);
        Assert.That(asked.Select(json => ManagedClientTestHooks.ULong(json, "id")), Is.EqualTo(new ulong?[] { 1, 2 }));
    }

    [Test]
    public async Task ClientLeavesAfterAJoinTheNodeDidNotAnswerInTime()
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://127.0.0.1:" + port + "/"); listener.Start();
        var asked = new List<string>();
        _ = AnswerAJoinGivenUp(listener, asked);
        using var client = await ManagedClient.ConnectAsync("http://127.0.0.1:" + port, "grant");

        var late = Assert.ThrowsAsync<NetException>(() => client.JoinRawAsync("{\"t\":\"join\",\"name\":\"silent\"}", 300));
        Assert.That(late.Message, Is.EqualTo("timeout waiting for joined"));
        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (asked.Count < 2 && clock.ElapsedMilliseconds < 2000) await Task.Delay(10);
        Assert.That(asked.Select(json => ManagedClientTestHooks.Str(json, "t")), Is.EqualTo(new[] { "join", "leave" }));
    }

    [TestCase(0)]
    [TestCase(50)]
    public async Task AJoinTheNodeEndsWithAKickFailsAtOnceWithTheReason(int kickAfter)
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://127.0.0.1:" + port + "/"); listener.Start();
        var asked = new List<string>();
        _ = AnswerAJoinGivenUp(listener, asked);
        using var client = await ManagedClient.ConnectAsync("http://127.0.0.1:" + port, "grant");
        var lefts = new List<string>();
        client.OnControl += json => { if (ManagedClientTestHooks.Str(json, "t") == "left") lefts.Add(json); };

        var clock = System.Diagnostics.Stopwatch.StartNew();
        var kicked = Assert.ThrowsAsync<NetException>(() => client.JoinRawAsync("{\"t\":\"join\",\"name\":\"blacklist\",\"kick_after\":" + kickAfter + "}"));
        Assert.That(kicked.Message, Is.EqualTo("left: cheater"));
        Assert.That(clock.ElapsedMilliseconds, Is.LessThan(1000));
        Assert.That(client.JoinedJson, Is.Null);
        for (int i = 0; i < 30; i++) { await Task.Delay(10); client.Poll(); }
        Assert.That(lefts, Is.Empty, "the game hears of no room it was never in");
        Assert.That(asked.Select(json => ManagedClientTestHooks.Str(json, "t")), Is.EqualTo(new[] { "join" }));
    }
}
