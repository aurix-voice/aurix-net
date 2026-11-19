// Deterministic bot kit, a port of `net-sim-bots`: behavior trees, hierarchical state machines and
// an integer blackboard. Trees/machines load from the same JSON the Rust crate emits (serde's
// externally tagged enums); tick hashes match Rust bit for bit.
using System;
using System.Collections.Generic;
using System.Linq;
using Newtonsoft.Json.Linq;

namespace Aurix.Net.Sim.Bots
{
    public enum Status
    {
        Running,
        Success,
        Failure,
    }

    public sealed class Blackboard
    {
        public readonly SortedDictionary<uint, long> Values = new SortedDictionary<uint, long>();

        public long Get(uint key) => Values.TryGetValue(key, out var value) ? value : 0;

        public void Set(uint key, long value) => Values[key] = value;

        public bool Has(uint key) => Values.ContainsKey(key);

        public void Clear(uint key) => Values.Remove(key);
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
        public uint Id; // Condition/Action id, Wait ticks, Parallel need, Repeat times, Cooldown/Timeout ticks
        public uint Key;
        public byte Op;
        public long Value; // Compare/Set

        public static BtNode Parse(string json) => Parse(JToken.Parse(json));

        public static BtNode Parse(JToken token)
        {
            var prop = ((JObject)token).Properties().First();
            var node = new BtNode { Kind = prop.Name };
            var value = prop.Value;
            switch (node.Kind)
            {
                case "Sequence":
                case "Selector":
                    foreach (var child in (JArray)value)
                    {
                        node.Children.Add(Parse(child));
                    }

                    break;
                case "Parallel":
                    node.Id = (uint)value["need"];
                    foreach (var child in (JArray)value["children"])
                    {
                        node.Children.Add(Parse(child));
                    }

                    break;
                case "Condition":
                case "Action":
                case "Wait":
                    node.Id = (uint)value;
                    break;
                case "Inverter":
                case "Succeeder":
                    node.Children.Add(Parse(value));
                    break;
                case "Repeat":
                    node.Id = (uint)value["times"];
                    node.Children.Add(Parse(value["child"]));
                    break;
                case "Cooldown":
                case "Timeout":
                    node.Id = (uint)value["ticks"];
                    node.Children.Add(Parse(value["child"]));
                    break;
                case "Compare":
                    node.Key = (uint)value["key"];
                    node.Op = (byte)value["op"];
                    node.Value = (long)value["value"];
                    break;
                case "Set":
                    node.Key = (uint)value["key"];
                    node.Value = (long)value["value"];
                    break;
                default:
                    throw new ArgumentException("unknown node " + node.Kind);
            }
            return node;
        }

        public int Count() => 1 + Children.Sum(child => child.Count());
    }

    public sealed class BehaviorTree
    {
        struct NodeState
        {
            public uint Counter,
                Cooldown;
            public bool Running;
        }

        public readonly BtNode Root;
        readonly NodeState[] _states;
        public uint Tick { get; private set; }

        public BehaviorTree(BtNode root)
        {
            Root = root;
            _states = new NodeState[root.Count()];
        }

        public static BehaviorTree FromJson(string json) => new BehaviorTree(BtNode.Parse(json));

        public Status Step(IBrain brain, Blackboard bb)
        {
            var st = Run(Root, 0, brain, bb);
            for (int i = 0; i < _states.Length; i++)
            {
                if (_states[i].Cooldown > 0)
                {
                    _states[i].Cooldown--;
                }
            }

            Tick++;
            return st;
        }

        public void Reset(IBrain brain, Blackboard bb)
        {
            Abort(Root, 0, brain, bb);
            Array.Clear(_states, 0, _states.Length);
        }

        public ulong Hash()
        {
            ulong hash = 0xcbf29ce484222325UL;
            void Mix(ulong value)
            {
                for (int i = 0; i < 8; i++)
                {
                    hash ^= value & 0xff;
                    hash *= 0x100000001b3UL;
                    value >>= 8;
                }
            }
            Mix(Tick);
            foreach (var state in _states)
            {
                Mix(
                    (ulong)state.Counter
                        | ((ulong)state.Cooldown << 32)
                        | (state.Running ? 1UL << 63 : 0)
                );
            }

            return hash;
        }

        Status Run(BtNode node, int idx, IBrain brain, Blackboard bb)
        {
            ref var state = ref _states[idx];
            switch (node.Kind)
            {
                case "Condition":
                    return brain.Condition(node.Id, bb) ? Status.Success : Status.Failure;
                case "Action":
                {
                    var st = brain.Action(node.Id, bb);
                    state.Running = st == Status.Running;
                    return st;
                }
                case "Compare":
                {
                    long value = bb.Get(node.Key),
                        x = node.Value;
                    bool ok = node.Op switch
                    {
                        0 => value == x,
                        1 => value != x,
                        2 => value < x,
                        3 => value <= x,
                        4 => value > x,
                        _ => value >= x,
                    };
                    return ok ? Status.Success : Status.Failure;
                }
                case "Set":
                    bb.Set(node.Key, node.Value);
                    return Status.Success;
                case "Wait":
                    state.Counter++;
                    if (state.Counter >= node.Id)
                    {
                        state.Counter = 0;
                        state.Running = false;
                        return Status.Success;
                    }
                    state.Running = true;
                    return Status.Running;
                case "Sequence":
                case "Selector":
                {
                    bool isSeq = node.Kind == "Sequence";
                    uint start = state.Counter;
                    int childIdx = idx + 1;
                    var result = isSeq ? Status.Success : Status.Failure;
                    for (int i = 0; i < node.Children.Count; i++)
                    {
                        var child = node.Children[i];
                        if (i < start)
                        {
                            if (child.Kind == "Condition" || child.Kind == "Compare")
                            {
                                var status = Run(child, childIdx, brain, bb);
                                if (
                                    (isSeq && status == Status.Failure)
                                    || (!isSeq && status == Status.Success)
                                )
                                {
                                    int rest = childIdx + child.Count();
                                    for (int j = i + 1; j < node.Children.Count; j++)
                                    {
                                        Abort(node.Children[j], rest, brain, bb);
                                        rest += node.Children[j].Count();
                                    }
                                    _states[idx].Counter = 0;
                                    _states[idx].Running = false;
                                    return status;
                                }
                            }
                            childIdx += child.Count();
                            continue;
                        }
                        var st = Run(child, childIdx, brain, bb);
                        childIdx += child.Count();
                        if (st == Status.Running)
                        {
                            _states[idx].Counter = (uint)i;
                            _states[idx].Running = true;
                            int rest = childIdx;
                            for (int j = i + 1; j < node.Children.Count; j++)
                            {
                                Abort(node.Children[j], rest, brain, bb);
                                rest += node.Children[j].Count();
                            }
                            return Status.Running;
                        }
                        if (!isSeq && st == Status.Success)
                        {
                            result = Status.Success;
                            break;
                        }
                        if (isSeq && st == Status.Failure)
                        {
                            result = Status.Failure;
                            break;
                        }
                    }
                    _states[idx].Counter = 0;
                    _states[idx].Running = false;
                    return result;
                }
                case "Parallel":
                {
                    int childIdx = idx + 1;
                    uint ok = 0,
                        failed = 0;
                    foreach (var child in node.Children)
                    {
                        var st = Run(child, childIdx, brain, bb);
                        childIdx += child.Count();
                        if (st == Status.Success)
                        {
                            ok++;
                        }
                        else if (st == Status.Failure)
                        {
                            failed++;
                        }
                    }
                    if (ok >= node.Id)
                    {
                        AbortChildren(node, idx, brain, bb);
                        return Status.Success;
                    }
                    if ((uint)node.Children.Count - failed < node.Id)
                    {
                        AbortChildren(node, idx, brain, bb);
                        return Status.Failure;
                    }
                    return Status.Running;
                }
                case "Inverter":
                {
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    return st == Status.Success ? Status.Failure
                        : st == Status.Failure ? Status.Success
                        : Status.Running;
                }
                case "Succeeder":
                {
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    return st == Status.Running ? Status.Running : Status.Success;
                }
                case "Repeat":
                {
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    if (st == Status.Running)
                    {
                        return Status.Running;
                    }

                    if (st == Status.Failure)
                    {
                        _states[idx].Counter = 0;
                        return Status.Failure;
                    }
                    _states[idx].Counter++;
                    if (node.Id > 0 && _states[idx].Counter >= node.Id)
                    {
                        _states[idx].Counter = 0;
                        return Status.Success;
                    }
                    return Status.Running;
                }
                case "Cooldown":
                {
                    if (state.Cooldown > 0 && !state.Running)
                    {
                        return Status.Failure;
                    }

                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    _states[idx].Running = st == Status.Running;
                    if (st != Status.Running)
                    {
                        _states[idx].Cooldown = node.Id + 1;
                    }

                    return st;
                }
                case "Timeout":
                {
                    var st = Run(node.Children[0], idx + 1, brain, bb);
                    if (st == Status.Running)
                    {
                        _states[idx].Counter++;
                        if (_states[idx].Counter >= node.Id)
                        {
                            _states[idx].Counter = 0;
                            Abort(node.Children[0], idx + 1, brain, bb);
                            return Status.Failure;
                        }
                        return Status.Running;
                    }
                    _states[idx].Counter = 0;
                    return st;
                }
            }
            throw new InvalidOperationException("unknown node " + node.Kind);
        }

        void AbortChildren(BtNode node, int idx, IBrain brain, Blackboard bb)
        {
            int childIdx = idx + 1;
            foreach (var child in node.Children)
            {
                Abort(child, childIdx, brain, bb);
                childIdx += child.Count();
            }
        }

        void Abort(BtNode node, int idx, IBrain brain, Blackboard bb)
        {
            if (node.Kind == "Action")
            {
                if (_states[idx].Running)
                {
                    brain.Abort(node.Id, bb);
                }
            }
            else
            {
                AbortChildren(node, idx, brain, bb);
            }

            _states[idx].Counter = 0;
            _states[idx].Running = false;
        }
    }

    public sealed class Transition
    {
        public uint? Condition;
        public uint To;
    }

    public sealed class State
    {
        public uint Id;
        public uint? Parent,
            Initial,
            OnEnter,
            OnExit,
            Action;
        public List<Transition> Transitions = new List<Transition>();
    }

    public sealed class StateMachine
    {
        public readonly SortedDictionary<uint, State> States = new SortedDictionary<uint, State>();
        public uint Current;
        public uint TicksInState;
        public uint Tick;
        Status _lastAction = Status.Running;

        public StateMachine(IEnumerable<State> states, uint initial)
        {
            foreach (var state in states)
            {
                States[state.Id] = state;
            }

            Current = initial;
        }

        /// Accepts the Rust `StateMachine` JSON (`states` object keyed by id).
        public static StateMachine FromJson(string json)
        {
            var root = JObject.Parse(json);
            static uint? Opt(JToken token, string key) =>
                token[key] is JValue { Type: JTokenType.Integer } value ? (uint)value : (uint?)null;
            var states = new List<State>();
            foreach (var property in ((JObject)root["states"]).Properties())
            {
                var entry = property.Value;
                var state = new State
                {
                    Id = (uint)entry["id"],
                    Parent = Opt(entry, "parent"),
                    Initial = Opt(entry, "initial"),
                    OnEnter = Opt(entry, "on_enter"),
                    OnExit = Opt(entry, "on_exit"),
                    Action = Opt(entry, "action"),
                };
                foreach (var transition in (JArray)entry["transitions"])
                {
                    state.Transitions.Add(
                        new Transition
                        {
                            Condition = Opt(transition, "condition"),
                            To = (uint)transition["to"],
                        }
                    );
                }

                states.Add(state);
            }
            var machine = new StateMachine(states, (uint)root["current"]);
            machine.TicksInState = Opt(root, "ticks_in_state") ?? 0;
            machine.Tick = Opt(root, "tick") ?? 0;
            if (root["last_action"] is JValue { Type: JTokenType.String } la)
            {
                machine._lastAction = Enum.Parse<Status>((string)la);
            }

            return machine;
        }

        public List<uint> Ancestors(uint id)
        {
            var path = new List<uint> { id };
            var parent = States.TryGetValue(id, out var state) ? state.Parent : null;
            while (parent.HasValue)
            {
                path.Add(parent.Value);
                parent = States.TryGetValue(parent.Value, out var ps) ? ps.Parent : null;
            }
            return path;
        }

        public bool IsIn(uint id) => Ancestors(Current).Contains(id);

        public void Start(IBrain brain, Blackboard bb)
        {
            uint cur = Current;
            var chain = Ancestors(cur);
            chain.Reverse();
            foreach (var id in chain)
            {
                var action = States[id].OnEnter;
                if (action.HasValue)
                {
                    brain.Action(action.Value, bb);
                }
            }
            while (States.TryGetValue(cur, out var st) && st.Initial.HasValue)
            {
                var action = States[st.Initial.Value].OnEnter;
                if (action.HasValue)
                {
                    brain.Action(action.Value, bb);
                }

                cur = st.Initial.Value;
            }
            Current = cur;
            TicksInState = 0;
        }

        public void Step(IBrain brain, Blackboard bb)
        {
            uint? target = null;
            foreach (var id in Ancestors(Current))
            {
                foreach (var transition in States[id].Transitions)
                {
                    bool fire = transition.Condition.HasValue
                        ? brain.Condition(transition.Condition.Value, bb)
                        : _lastAction != Status.Running && id == Current;
                    if (fire)
                    {
                        target = transition.To;
                        break;
                    }
                }
                if (target.HasValue)
                {
                    break;
                }
            }
            if (target.HasValue)
            {
                DoTransition(target.Value, brain, bb);
            }

            var act = States.TryGetValue(Current, out var cs) ? cs.Action : null;
            _lastAction = act.HasValue ? brain.Action(act.Value, bb) : Status.Success;
            TicksInState++;
            Tick++;
        }

        public void DoTransition(uint to, IBrain brain, Blackboard bb)
        {
            var from = Ancestors(Current);
            var toChain = Ancestors(to);
            foreach (var id in from)
            {
                if (toChain.Contains(id))
                {
                    break;
                }

                var action = States[id].OnExit;
                if (action.HasValue)
                {
                    brain.Action(action.Value, bb);
                }
            }
            var enter = new List<uint>(toChain);
            enter.Reverse();
            foreach (var id in enter)
            {
                if (from.Contains(id))
                {
                    continue;
                }

                var action = States[id].OnEnter;
                if (action.HasValue)
                {
                    brain.Action(action.Value, bb);
                }
            }
            uint cur = to;
            while (States.TryGetValue(cur, out var st) && st.Initial.HasValue)
            {
                var action = States[st.Initial.Value].OnEnter;
                if (action.HasValue)
                {
                    brain.Action(action.Value, bb);
                }

                cur = st.Initial.Value;
            }
            Current = cur;
            TicksInState = 0;
            _lastAction = Status.Running;
        }

        public ulong Hash() =>
            (ulong)Current
            ^ ((ulong)TicksInState << 32)
            ^ unchecked((ulong)Tick * 0x9e3779b97f4a7c15UL);
    }
}
