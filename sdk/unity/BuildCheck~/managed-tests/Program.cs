// Offline unit checks for the managed core (no node needed): `dotnet run`.
using System;
using System.Collections.Generic;
using System.Linq;
using Aurix.Net;
using Aurix.Net.Managed;
using Aurix.Net.Sim;
using Aurix.Net.Sim.Bots;
using System.Text.Json;
using System.IO;

static class Program
{
    static int _fails;
    static void Check(bool ok, string what) { if (!ok) { _fails++; Console.WriteLine("FAIL " + what); } else Console.WriteLine("ok   " + what); }

    // 1-D mover: input byte = signed velocity, world is a wall at x = 10.
    static int Step(int x, byte[] input) { int v = input != null && input.Length > 0 ? (sbyte)input[0] : 0; return Math.Min(10, x + v); }
    static byte[] In(int v) => new[] { (byte)(sbyte)v };

    static void TickTimerTests()
    {
        var t = TickTimer.CreateFromSeconds(100, 0.5f, 60);
        Check(t.IsRunning && t.End == 130 && !t.Expired(100) && t.IsActive(129) && t.RemainingTicks(110) == 20, "0.5 s at 60 Hz = 30 ticks");
        Check(t.Expired(130) && t.Expired(500) && !t.IsActive(130) && t.RemainingTicks(130) == 0, "expired at End and stays expired");
        Check(System.Math.Abs(t.Normalized(115) - 0.5f) < 1e-6 && t.Normalized(999) == 1f, "normalized progress");
        Check(!TickTimer.None.IsRunning && !TickTimer.None.Expired(10) && TickTimer.None.Normalized(10) == 1f, "None never runs or expires");
        Check(TickTimer.CreateFromTicks(5, 0).End == 6, "zero ticks still expires one tick later");
        Check(System.Math.Abs(t.RemainingSeconds(100, 60) - 0.5f) < 1e-6, "remaining seconds");
    }

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


    sealed class Toy : ISimulation<(ulong state, int steps)>
    {
        public ulong State; public int Steps;
        public void Step(uint tick, Dictionary<ushort, byte[]> inputs)
        {
            foreach (var kv in inputs.OrderBy(k => k.Key)) { ulong b = kv.Value.Length > 0 ? kv.Value[0] : 0UL; unchecked { State = State * 31 + tick * 1000UL + kv.Key * 10UL + b; } }
            unchecked { State = State * 7 + tick; } Steps++;
        }
        public (ulong state, int steps) Snapshot() => (State, Steps);
        public void Restore((ulong state, int steps) s) { State = s.state; Steps = s.steps; }
    }
    static Dictionary<ushort, byte[]> Inputs(params (ushort, byte)[] v) { var d = new Dictionary<ushort, byte[]>(); foreach (var (a, b) in v) d[a] = new[] { b }; return d; }
    static ulong Reference(List<Dictionary<ushort, byte[]>> script) { var s = new Toy(); for (int t = 0; t < script.Count; t++) s.Step((uint)t, script[t]); return s.State; }

    static void RollbackTests()
    {
        var sim = new Toy(); var rb = new LockstepRollback<(ulong, int)>(sim, 1, 0, 8);
        var script = new List<Dictionary<ushort, byte[]>>(); for (int i = 0; i < 10; i++) script.Add(Inputs((1, 5), (2, 9)));
        rb.Confirm(0, script[0]); bool rolled = false;
        for (uint t = 1; t < 10; t++) { rb.LocalInput(t, new byte[] { 5 }); rb.Predict(t + 3); rolled |= rb.Confirm(t, script[(int)t]); }
        Check(!rolled && rb.Rollbacks == 0 && rb.ConfirmedTick == 10 && rb.HeadTick > rb.ConfirmedTick, "rollback: correct predictions never roll back");

        sim = new Toy(); rb = new LockstepRollback<(ulong, int)>(sim, 1, 0, 8);
        script.Clear(); for (int i = 0; i < 8; i++) script.Add(Inputs((1, 1), (2, 2)));
        script[4] = Inputs((1, 1), (2, 7)); script[5] = Inputs((2, 7));
        for (uint t = 0; t < 8; t++) { rb.LocalInput(t, new byte[] { 1 }); rb.Predict(t + 4); rb.Confirm(t, script[(int)t]); }
        Check(rb.Rollbacks == 4 && rb.ResimulatedTicks > 0 && rb.HeadTick == 11, "rollback: mispredictions roll back and re-predict (" + rb.Rollbacks + ")");
        var pure = new Toy(); var rb2 = new LockstepRollback<(ulong, int)>(pure, 1, 0, 8);
        for (uint t = 0; t < 8; t++) rb2.Confirm(t, script[(int)t]);
        Check(pure.State == Reference(script), "rollback: confirmed-only run equals pure lockstep");

        sim = new Toy(); rb = new LockstepRollback<(ulong, int)>(sim, 1, 0, 3);
        rb.Predict(100);
        Check(rb.HeadTick == 3 && rb.PredictedTicks == 3 && !rb.Confirm(7, Inputs()), "rollback: prediction bounded, out-of-order confirm ignored");
        rb.Reset(50); rb.Predict(52);
        Check(rb.ConfirmedTick == 50 && rb.HeadTick == 52, "rollback: reset forgets history");

        var ring = new ReplayRing<uint>(50, 10);
        for (uint t = 0; t < 100; t++) ring.Record(t, Inputs((1, (byte)t)), () => t * 100);
        var w = ring.Window(73).Value;
        Check(w.Start == 70 && w.Snapshot == 7000 && w.Inputs.Count == 30 && w.Inputs[0][1][0] == 70, "replay ring: window from nearest snapshot");
        w = ring.Window(5).Value;
        Check(w.Start == 50 && w.Inputs.Count == 50 && ring.Oldest == 50, "replay ring: clamps to the oldest kept snapshot");
        ring.Record(200, Inputs(), () => 1);
        Check(ring.Oldest == 200 && ring.Window(0).Value.Inputs.Count == 1, "replay ring: gap resets");

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

    static void BotsTests()
    {
        var dir = AppContext.BaseDirectory;
        while (dir != null && !Directory.Exists(Path.Combine(dir, "crates"))) dir = Path.GetDirectoryName(dir.TrimEnd(Path.DirectorySeparatorChar));
        var doc = JsonDocument.Parse(File.ReadAllText(Path.Combine(dir, "crates/net-sim-bots/fixtures/vectors.json"))).RootElement;
        var tree = new BehaviorTree(BtNode.Parse(doc.GetProperty("tree")));
        var bb = new Blackboard(); var brain = new Scripted { PatrolLeft = 2 };
        bool ok = true; uint t = 0;
        foreach (var exp in doc.GetProperty("tree_ticks").EnumerateArray())
        {
            brain.T = t; var st = tree.Step(brain, bb);
            ok &= st.ToString() == exp.GetProperty("status").GetString() && "0x" + tree.Hash().ToString("x") == exp.GetProperty("hash").GetString() && BbJson(bb) == BbJson(exp.GetProperty("bb"));
            t++;
        }
        Check(ok, "bots: behavior tree replays the Rust vectors (" + t + " ticks)");
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
        Check(fok, "bots: state machine replays the Rust vectors (" + t + " ticks)");
    }

    static void SimTests()
    {
        Check(SimWorld.GoldenHash() == 0x58d9870ba45952b5UL, "sim: golden hash equals the Rust/wasm value");
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
        Check(a.Hash() == b.Hash() && ballA == ballB, "sim: two worlds agree");
        double y = a.Body(ballA).Y;
        Check(y > 0.9 && y < 1.1, "sim: ball rests on the floor (" + y.ToString("F3") + ")");
        var snap = a.Snapshot(); ulong h = a.Hash(); a.Step();
        Check(a.Restore(snap) && a.Hash() == h, "sim: snapshot round-trip");
        Check(a.Raycast(0, 5, 0, -10, out var hit) && hit.Body == ballA && Math.Abs(hit.Ny - 1) < 1e-6, "sim: raycast hits the ball from above");
        Check(SimWorld.NavGoldenHash() == 0x746c5a608abe2f26UL, "nav: golden hash equals the Rust/wasm value");
        Check(SimWorld.JointGoldenHash() == 0x4bcc781a484ceeedUL, "joints: golden hash equals the Rust/wasm value");
        Check(Sim3World.GoldenHash() == 0x0a9c7e6fc4fa43bfUL, "sim3: golden hash equals the Rust/wasm value");
        using (var w3 = new Sim3World())
        {
            w3.AddBox(BodyKind.Static, new Vec3(0, -0.5, 0), new Vec3(10, 0.5, 10));
            uint ball3 = w3.AddSphere(BodyKind.Dynamic, new Vec3(0, 3, 0), 0.5);
            for (int i = 0; i < 240; i++) w3.Step();
            Check(Math.Abs(w3.Body(ball3).Position.Y - 0.5) < 0.03, "sim3: sphere rests on the floor");
            var snap3 = w3.Snapshot(); ulong h3 = w3.Hash();
            w3.SetPosition(ball3, new Vec3(0, 9, 0));
            Check(w3.Hash() != h3 && w3.Restore(snap3) && w3.Hash() == h3, "sim3: snapshot round-trip");
            Check(w3.Raycast(new Vec3(0, 5, 0), new Vec3(0, -10, 0), out var h3hit) && h3hit.Body == ball3 && Math.Abs(h3hit.Point.Y - 1) < 0.05, "sim3: raycast hits the sphere");
        }
        Check(Sim3World.JointGoldenHash() == 0x5db56af63cceee85UL, "sim3: joint golden hash equals the Rust/wasm value");
        Check(Sim3World.HullGoldenHash() == 0xd656fc8f55999cf8UL, "sim3: hull golden hash equals the Rust/wasm value");
        using (var c3 = new Sim3World())
        {
            c3.AddBox(BodyKind.Static, new Vec3(0, -0.5, 0), new Vec3(20, 0.5, 20));
            var verts = new[] { new Vec3(-0.5, -0.25, -0.5), new Vec3(0.5, -0.25, -0.5), new Vec3(0.5, -0.25, 0.5), new Vec3(-0.5, -0.25, 0.5), new Vec3(0, 0.75, 0) };
            var faces = new[] { new uint[] { 0, 1, 2, 3 }, new uint[] { 0, 1, 4 }, new uint[] { 1, 2, 4 }, new uint[] { 2, 3, 4 }, new uint[] { 3, 0, 4 } };
            uint pyr = c3.AddConvex(BodyKind.Dynamic, new Vec3(0, 2, 0), verts, faces);
            for (int i = 0; i < 300; i++) c3.Step();
            var pp = c3.Body(pyr).Position;
            Check(Math.Abs(pp.Y - 0.25) < 0.06, "sim3: convex pyramid rests on the floor (" + pp.Y.ToString("F3") + ")");
            Check(c3.SphereCast(new Vec3(0, 5, 0), new Vec3(0, -10, 0), 0.5, out var sh) && sh.Body == pyr && Math.Abs(sh.Point.Y - 1.0) < 0.05 && sh.Normal.Y > 0.5, "sim3: sphere cast hits the pyramid apex");
            var q0 = new Quat(0, 0, 0, 1);
            Check(c3.BoxCast(new Vec3(0, 5, 0), q0, new Vec3(0.5, 0.5, 0.5), new Vec3(0, -10, 0), out var bh) && bh.Body == pyr && Math.Abs(bh.Point.Y - 1.0) < 0.05 && Math.Abs(bh.Fraction - 0.35) < 0.01, "sim3: box cast stops on the pyramid apex (y=" + bh.Point.Y.ToString("F3") + ")");
            Check(c3.CapsuleCast(new Vec3(0, 5, 0), q0, 0.5, 0.25, new Vec3(0, -10, 0), out var ch) && ch.Body == pyr && Math.Abs(ch.Point.Y - 1.0) < 0.05 && Math.Abs(ch.Fraction - 0.325) < 0.01, "sim3: capsule cast stops on the pyramid apex (y=" + ch.Point.Y.ToString("F3") + ")");
            Check(!c3.BoxCast(new Vec3(0, 5, 0), q0, new Vec3(0.5, 0.5, 0.5), new Vec3(10, 0, 0), out _), "sim3: box cast misses sideways");
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
            Check(angle > 0.3 && angle < Math.PI / 2 + 0.1, "sim3: hinged door opened within its limits (" + angle.ToString("F2") + ")");
            var hp = j3.Body(hero).Position;
            Check(hp.X > 5 && Math.Abs(hp.Y - 0.8) < 0.05 && j3.CharacterState(k).Grounded, "sim3: character stepped over the ledge and stands (" + hp.X.ToString("F2") + "," + hp.Y.ToString("F2") + ")");
        }
        using (var j = new SimWorld(0, -10))
        {
            uint floor = j.AddBox(BodyKind.Static, 0, -0.5, 10, 0.5);
            uint ball = j.AddCircle(BodyKind.Dynamic, 0, 2, 0.5);
            int begins = 0;
            for (int i = 0; i < 120; i++) { j.Step(); foreach (var e in j.Events()) if (e.Begin && ((e.A == ball && e.B == floor) || (e.A == floor && e.B == ball))) begins++; }
            Check(begins >= 1, "joints: ball/floor contact begin reported");
            Check(j.CircleCast(-5, 0.5, 10, 0, 0.4, out var ch) && ch.Body == ball && Math.Abs(ch.X + 0.9) < 0.01, "joints: circle cast stops one radius short of the ball");
        }
        using var n = new SimWorld(0, 0);
        n.NavGrid(0, 0, 1, 8, 6, new (uint, uint)[] { (4, 0), (4, 1), (4, 2), (4, 3), (4, 4) });
        var path = n.NavPath(0.5, 0.5, 7.5, 0.5);
        Check(path != null && Array.Exists(path, p => p.x == 4 && p.y == 5), "nav: path rounds the wall");
        Check(n.NavRaycast(0.5, 0.5, 7.5, 0.5, out _, out _), "nav: wall blocks line of sight");
        uint field = n.NavField(7.5, 0.5);
        uint ag = n.AddAgent(0.5, 0.5, 0.25, 3);
        n.AgentFollow(ag, field);
        for (int i = 0; i < 600; i++) n.CrowdStep();
        var st = n.Agent(ag);
        Check(Math.Sqrt((st.X - 7.5) * (st.X - 7.5) + (st.Y - 0.5) * (st.Y - 0.5)) < 0.5, "nav: agent reached the goal (" + st.X.ToString("F2") + "," + st.Y.ToString("F2") + ")");
    }

    static int Main()
    {
        try { SimTests(); }
        catch (DllNotFoundException)
        {
            Console.WriteLine("skip sim/nav checks: libnet_sim_wasm not built (cargo build --release -p net-sim-wasm)");
        }
        BotsTests();
        PredictorTests();
        TickTimerTests();
        LockstepTests();
        RollbackTests();
        CodecVectors();
        Console.WriteLine(_fails == 0 ? "all managed checks passed" : _fails + " check(s) failed");
        return _fails == 0 ? 0 : 1;
    }
}
