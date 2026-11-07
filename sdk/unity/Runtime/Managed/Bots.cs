// Deterministic bot kit — port of `net-sim-bots`: behavior trees, hierarchical state machines and
// an integer blackboard. Trees/machines load from the same JSON the Rust crate emits (serde's
// externally tagged enums); tick hashes match Rust bit for bit.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;

namespace Aurix.Net.Sim.Bots
{
    public enum Status { Running, Success, Failure }

    public sealed class Blackboard
    {
        public readonly SortedDictionary<uint, long> Values = new SortedDictionary<uint, long>();
        public long Get(uint k) => Values.TryGetValue(k, out var v) ? v : 0;
        public void Set(uint k, long v) => Values[k] = v;
        public bool Has(uint k) => Values.ContainsKey(k);
        public void Clear(uint k) => Values.Remove(k);
    }

    public interface IBrain
    {
        bool Condition(uint id, Blackboard bb);
        Status Action(uint id, Blackboard bb);
        void Abort(uint id, Blackboard bb);
    }

    /// Tree node in serde's externally tagged JSON shape.
    public sealed class BtNode
    {
        public string Kind;
        public List<BtNode> Children = new List<BtNode>();
        public uint Id;        // Condition/Action id, Wait ticks, Parallel need, Repeat times, Cooldown/Timeout ticks
        public uint Key; public byte Op; public long Value; // Compare/Set

        public static BtNode Parse(string json) => Parse(JsonDocument.Parse(json).RootElement);
        public static BtNode Parse(JsonElement e)
        {
            var prop = e.EnumerateObject().First();
            var n = new BtNode { Kind = prop.Name };
            var v = prop.Value;
            switch (n.Kind)
            {
                case "Sequence": case "Selector": foreach (var c in v.EnumerateArray()) n.Children.Add(Parse(c)); break;
                case "Parallel": n.Id = v.GetProperty("need").GetUInt32(); foreach (var c in v.GetProperty("children").EnumerateArray()) n.Children.Add(Parse(c)); break;
                case "Condition": case "Action": case "Wait": n.Id = v.GetUInt32(); break;
                case "Inverter": case "Succeeder": n.Children.Add(Parse(v)); break;
                case "Repeat": n.Id = v.GetProperty("times").GetUInt32(); n.Children.Add(Parse(v.GetProperty("child"))); break;
                case "Cooldown": case "Timeout": n.Id = v.GetProperty("ticks").GetUInt32(); n.Children.Add(Parse(v.GetProperty("child"))); break;
                case "Compare": n.Key = v.GetProperty("key").GetUInt32(); n.Op = v.GetProperty("op").GetByte(); n.Value = v.GetProperty("value").GetInt64(); break;
                case "Set": n.Key = v.GetProperty("key").GetUInt32(); n.Value = v.GetProperty("value").GetInt64(); break;
                default: throw new ArgumentException("unknown node " + n.Kind);
            }
            return n;
        }
        public int Count() => 1 + Children.Sum(c => c.Count());
    }

    public sealed class BehaviorTree
    {
        struct NodeState { public uint Counter, Cooldown; public bool Running; }
        public readonly BtNode Root;
        readonly NodeState[] _states;
        public uint Tick { get; private set; }

        public BehaviorTree(BtNode root) { Root = root; _states = new NodeState[root.Count()]; }
        public static BehaviorTree FromJson(string json) => new BehaviorTree(BtNode.Parse(json));

        public Status Step(IBrain brain, Blackboard bb)
        {
            var st = Run(Root, 0, brain, bb);
            for (int i = 0; i < _states.Length; i++) if (_states[i].Cooldown > 0) _states[i].Cooldown--;
            Tick++;
            return st;
        }
        public void Reset(IBrain brain, Blackboard bb) { Abort(Root, 0, brain, bb); Array.Clear(_states, 0, _states.Length); }

        public ulong Hash()
        {
            ulong h = 0xcbf29ce484222325UL;
            void Mix(ulong v) { for (int i = 0; i < 8; i++) { h ^= v & 0xff; h *= 0x100000001b3UL; v >>= 8; } }
            Mix(Tick);
            foreach (var s in _states) Mix((ulong)s.Counter | ((ulong)s.Cooldown << 32) | (s.Running ? 1UL << 63 : 0));
            return h;
        }

        Status Run(BtNode node, int idx, IBrain brain, Blackboard bb)
        {
            ref var s = ref _states[idx];
            switch (node.Kind)
            {
                case "Condition": return brain.Condition(node.Id, bb) ? Status.Success : Status.Failure;
                case "Action": { var st = brain.Action(node.Id, bb); s.Running = st == Status.Running; return st; }
                case "Compare":
                {
                    long v = bb.Get(node.Key), x = node.Value;
                    bool ok = node.Op switch { 0 => v == x, 1 => v != x, 2 => v < x, 3 => v <= x, 4 => v > x, _ => v >= x };
                    return ok ? Status.Success : Status.Failure;
                }
                case "Set": bb.Set(node.Key, node.Value); return Status.Success;
                case "Wait":
                    s.Counter++;
                    if (s.Counter >= node.Id) { s.Counter = 0; s.Running = false; return Status.Success; }
                    s.Running = true; return Status.Running;
                case "Sequence": case "Selector":
                {
                    bool isSeq = node.Kind == "Sequence";
                    uint start = s.Counter;
                    int childIdx = idx + 1;
                    var result = isSeq ? Status.Success : Status.Failure;
                    for (int i = 0; i < node.Children.Count; i++)
                    {
                        var c = node.Children[i];
                        if (i < start)
                        {
                            if (c.Kind == "Condition" || c.Kind == "Compare")
                            {
                                var g = Run(c, childIdx, brain, bb);
                                if ((isSeq && g == Status.Failure) || (!isSeq && g == Status.Success))
                                {
                                    int rest = childIdx + c.Count();
                                    for (int j = i + 1; j < node.Children.Count; j++) { Abort(node.Children[j], rest, brain, bb); rest += node.Children[j].Count(); }
                                    _states[idx].Counter = 0; _states[idx].Running = false;
                                    return g;
                                }
                            }
                            childIdx += c.Count();
                            continue;
                        }
                        var st = Run(c, childIdx, brain, bb);
                        childIdx += c.Count();
                        if (st == Status.Running)
                        {
                            _states[idx].Counter = (uint)i; _states[idx].Running = true;
                            int rest = childIdx;
                            for (int j = i + 1; j < node.Children.Count; j++) { Abort(node.Children[j], rest, brain, bb); rest += node.Children[j].Count(); }
                            return Status.Running;
                        }
                        if (!isSeq && st == Status.Success) { result = Status.Success; break; }
                        if (isSeq && st == Status.Failure) { result = Status.Failure; break; }
                    }
                    _states[idx].Counter = 0; _states[idx].Running = false;
                    return result;
                }
                case "Parallel":
                {
                    int childIdx = idx + 1; uint ok = 0, failed = 0;
                    foreach (var c in node.Children)
                    {
                        var st = Run(c, childIdx, brain, bb); childIdx += c.Count();
                        if (st == Status.Success) ok++; else if (st == Status.Failure) failed++;
                    }
                    if (ok >= node.Id) { AbortChildren(node, idx, brain, bb); return Status.Success; }
                    if ((uint)node.Children.Count - failed < node.Id) { AbortChildren(node, idx, brain, bb); return Status.Failure; }
                    return Status.Running;
                }
                case "Inverter": { var st = Run(node.Children[0], idx + 1, brain, bb); return st == Status.Success ? Status.Failure : st == Status.Failure ? Status.Success : Status.Running; }
                case "Succeeder": { var st = Run(node.Children[0], idx + 1, brain, bb); return st == Status.Running ? Status.Running : Status.Success; }
                case "Repeat":
                {
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    if (st == Status.Running) return Status.Running;
                    if (st == Status.Failure) { _states[idx].Counter = 0; return Status.Failure; }
                    _states[idx].Counter++;
                    if (node.Id > 0 && _states[idx].Counter >= node.Id) { _states[idx].Counter = 0; return Status.Success; }
                    return Status.Running;
                }
                case "Cooldown":
                {
                    if (s.Cooldown > 0 && !s.Running) return Status.Failure;
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    _states[idx].Running = st == Status.Running;
                    if (st != Status.Running) _states[idx].Cooldown = node.Id + 1;
                    return st;
                }
                case "Timeout":
                {
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    if (st == Status.Running)
                    {
                        _states[idx].Counter++;
                        if (_states[idx].Counter >= node.Id) { _states[idx].Counter = 0; Abort(node.Children[0], idx + 1, brain, bb); return Status.Failure; }
                        return Status.Running;
                    }
                    _states[idx].Counter = 0; return st;
                }
            }
            throw new InvalidOperationException("unknown node " + node.Kind);
        }

        void AbortChildren(BtNode node, int idx, IBrain brain, Blackboard bb)
        {
            int childIdx = idx + 1;
            foreach (var c in node.Children) { Abort(c, childIdx, brain, bb); childIdx += c.Count(); }
        }
        void Abort(BtNode node, int idx, IBrain brain, Blackboard bb)
        {
            if (node.Kind == "Action") { if (_states[idx].Running) brain.Abort(node.Id, bb); }
            else AbortChildren(node, idx, brain, bb);
            _states[idx].Counter = 0; _states[idx].Running = false;
        }
    }

    public sealed class Transition { public uint? Condition; public uint To; }
    public sealed class State
    {
        public uint Id; public uint? Parent, Initial, OnEnter, OnExit, Action;
        public List<Transition> Transitions = new List<Transition>();
    }

    public sealed class StateMachine
    {
        public readonly SortedDictionary<uint, State> States = new SortedDictionary<uint, State>();
        public uint Current; public uint TicksInState; public uint Tick;
        Status _lastAction = Status.Running;

        public StateMachine(IEnumerable<State> states, uint initial) { foreach (var s in states) States[s.Id] = s; Current = initial; }
        /// Accepts the Rust `StateMachine` JSON (`states` object keyed by id).
        public static StateMachine FromJson(string json)
        {
            var d = JsonDocument.Parse(json).RootElement;
            static uint? Opt(JsonElement e, string k) => e.TryGetProperty(k, out var v) && v.ValueKind == JsonValueKind.Number ? v.GetUInt32() : (uint?)null;
            var states = new List<State>();
            foreach (var p in d.GetProperty("states").EnumerateObject())
            {
                var e = p.Value;
                var s = new State { Id = e.GetProperty("id").GetUInt32(), Parent = Opt(e, "parent"), Initial = Opt(e, "initial"), OnEnter = Opt(e, "on_enter"), OnExit = Opt(e, "on_exit"), Action = Opt(e, "action") };
                foreach (var t in e.GetProperty("transitions").EnumerateArray()) s.Transitions.Add(new Transition { Condition = Opt(t, "condition"), To = t.GetProperty("to").GetUInt32() });
                states.Add(s);
            }
            var m = new StateMachine(states, d.GetProperty("current").GetUInt32());
            m.TicksInState = Opt(d, "ticks_in_state") ?? 0; m.Tick = Opt(d, "tick") ?? 0;
            if (d.TryGetProperty("last_action", out var la)) m._lastAction = Enum.Parse<Status>(la.GetString());
            return m;
        }
        public List<uint> Ancestors(uint id)
        {
            var o = new List<uint> { id };
            var p = States.TryGetValue(id, out var s) ? s.Parent : null;
            while (p.HasValue) { o.Add(p.Value); p = States.TryGetValue(p.Value, out var ps) ? ps.Parent : null; }
            return o;
        }
        public bool IsIn(uint id) => Ancestors(Current).Contains(id);
        public void Start(IBrain brain, Blackboard bb)
        {
            uint cur = Current;
            var chain = Ancestors(cur); chain.Reverse();
            foreach (var s in chain) { var a = States[s].OnEnter; if (a.HasValue) brain.Action(a.Value, bb); }
            while (States.TryGetValue(cur, out var st) && st.Initial.HasValue) { var a = States[st.Initial.Value].OnEnter; if (a.HasValue) brain.Action(a.Value, bb); cur = st.Initial.Value; }
            Current = cur; TicksInState = 0;
        }
        public void Step(IBrain brain, Blackboard bb)
        {
            uint? target = null;
            foreach (var s in Ancestors(Current))
            {
                foreach (var t in States[s].Transitions)
                {
                    bool fire = t.Condition.HasValue ? brain.Condition(t.Condition.Value, bb) : _lastAction != Status.Running && s == Current;
                    if (fire) { target = t.To; break; }
                }
                if (target.HasValue) break;
            }
            if (target.HasValue) DoTransition(target.Value, brain, bb);
            var act = States.TryGetValue(Current, out var cs) ? cs.Action : null;
            _lastAction = act.HasValue ? brain.Action(act.Value, bb) : Status.Success;
            TicksInState++; Tick++;
        }
        public void DoTransition(uint to, IBrain brain, Blackboard bb)
        {
            var from = Ancestors(Current); var toChain = Ancestors(to);
            foreach (var s in from) { if (toChain.Contains(s)) break; var a = States[s].OnExit; if (a.HasValue) brain.Action(a.Value, bb); }
            var enter = new List<uint>(toChain); enter.Reverse();
            foreach (var s in enter) { if (from.Contains(s)) continue; var a = States[s].OnEnter; if (a.HasValue) brain.Action(a.Value, bb); }
            uint cur = to;
            while (States.TryGetValue(cur, out var st) && st.Initial.HasValue) { var a = States[st.Initial.Value].OnEnter; if (a.HasValue) brain.Action(a.Value, bb); cur = st.Initial.Value; }
            Current = cur; TicksInState = 0; _lastAction = Status.Running;
        }
        public ulong Hash() => (ulong)Current ^ ((ulong)TicksInState << 32) ^ unchecked((ulong)Tick * 0x9e3779b97f4a7c15UL);
    }
}
