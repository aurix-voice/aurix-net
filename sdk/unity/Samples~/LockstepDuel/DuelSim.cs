// Lockstep duel sample: a deterministic integer simulation stepped only from confirmed inputs.
// Every client runs the same code over the same inputs; the node (or your WASM logic with
// props.logic) checks the hashes. Scene: NetRunner (DefaultMode = Lockstep) + this component.
using System.Collections.Generic;
using Aurix.Net.Managed;
using Aurix.Net.Unity;
using UnityEngine;

public sealed class DuelSim : MonoBehaviour
{
    public NetRunner Runner; public GameObject FighterPrefab;
    // state: per actor x,y in millimetres and hp; all integer, so every platform agrees
    readonly SortedDictionary<ushort, int[]> _fighters = new SortedDictionary<ushort, int[]>();
    readonly Dictionary<ushort, GameObject> _gos = new Dictionary<ushort, GameObject>();
    uint _tick; byte _dir;

    void Start()
    {
        Runner.OnJoined += () =>
        {
            Runner.Lockstep.OnRestore += Restore;
            Runner.Lockstep.OnTick += Step;
            Runner.Lockstep.OnCheckpointRequested += t => Runner.Lockstep.SendCheckpoint(_tick, Hash(), Serialize());
        };
    }

    void Update()
    {
        if (Runner.Lockstep == null) return;
        _dir = (byte)((Input.GetKey(KeyCode.W) ? 1 : 0) | (Input.GetKey(KeyCode.S) ? 2 : 0) | (Input.GetKey(KeyCode.A) ? 4 : 0) | (Input.GetKey(KeyCode.D) ? 8 : 0) | (Input.GetKey(KeyCode.Space) ? 16 : 0));
        foreach (var kv in _fighters) { if (!_gos.TryGetValue(kv.Key, out var go)) { go = Instantiate(FighterPrefab); _gos[kv.Key] = go; } go.transform.position = new Vector3(kv.Value[0] / 1000f, 0, kv.Value[1] / 1000f); }
    }

    void FixedUpdate() { Runner.Lockstep?.SendInput(new[] { _dir }); }

    void Step(uint tick, Dictionary<ushort, byte[]> inputs)
    {
        foreach (var a in inputs.Keys) if (!_fighters.ContainsKey(a)) _fighters[a] = new[] { (int)a * 2000, 0, 100 };
        foreach (var kv in inputs)
        {
            var f = _fighters[kv.Key]; byte d = kv.Value.Length > 0 ? kv.Value[0] : (byte)0;
            if ((d & 1) != 0) f[1] += 50; if ((d & 2) != 0) f[1] -= 50; if ((d & 4) != 0) f[0] -= 50; if ((d & 8) != 0) f[0] += 50;
            if ((d & 16) != 0) foreach (var o in _fighters) if (o.Key != kv.Key && Mathf.Abs(o.Value[0] - f[0]) + Mathf.Abs(o.Value[1] - f[1]) < 1500) o.Value[2] = Mathf.Max(0, o.Value[2] - 1);
        }
        _tick = tick + 1;
        if (_tick % 10 == 0) Runner.Lockstep.ReportHash(_tick, Hash());
    }

    void Restore(uint tick, ulong hash, byte[] state)
    {
        _fighters.Clear(); _tick = tick;
        for (int o = 0; o + 14 <= state.Length; o += 14) _fighters[(ushort)((state[o] << 8) | state[o + 1])] = new[] { System.BitConverter.ToInt32(state, o + 2), System.BitConverter.ToInt32(state, o + 6), System.BitConverter.ToInt32(state, o + 10) };
    }
    byte[] Serialize()
    {
        var b = new byte[_fighters.Count * 14]; int o = 0;
        foreach (var kv in _fighters) { b[o] = (byte)(kv.Key >> 8); b[o + 1] = (byte)kv.Key; for (int i = 0; i < 3; i++) System.BitConverter.GetBytes(kv.Value[i]).CopyTo(b, o + 2 + i * 4); o += 14; }
        return b;
    }
    ulong Hash() => LockstepClient.Fnv1a(Serialize());
}
