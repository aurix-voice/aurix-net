// Offline unit checks for the managed core (no node needed): `dotnet run`.
using System;
using System.Collections.Generic;
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

    static int Main()
    {
        PredictorTests();
        Console.WriteLine(_fails == 0 ? "all managed checks passed" : _fails + " check(s) failed");
        return _fails == 0 ? 0 : 1;
    }
}
