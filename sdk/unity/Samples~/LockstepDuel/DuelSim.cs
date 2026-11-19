// Lockstep duel sample: a deterministic integer simulation stepped only from confirmed inputs.
// Every client runs the same code over the same inputs; the node (or your WASM logic with
// props.logic) checks the hashes. Scene: NetRunner (DefaultMode = Lockstep) + this component.
using System.Collections.Generic;
using Aurix.Net.Managed;
using Aurix.Net.Unity;
using UnityEngine;

public sealed class DuelSim : MonoBehaviour
{
    public NetRunner Runner;
    public GameObject FighterPrefab;

    // state: per actor x,y in millimetres and hp; all integer, so every platform agrees
    readonly SortedDictionary<ushort, int[]> _fighters = new SortedDictionary<ushort, int[]>();
    readonly Dictionary<ushort, GameObject> _gos = new Dictionary<ushort, GameObject>();
    uint _tick;
    byte _dir;

    void Start()
    {
        Runner.OnJoined += () =>
        {
            Runner.Lockstep.OnRestore += Restore;
            Runner.Lockstep.OnTick += Step;
            Runner.Lockstep.OnCheckpointRequested += tick =>
                Runner.Lockstep.SendCheckpoint(_tick, Hash(), Serialize());
        };
    }

    void Update()
    {
        if (Runner.Lockstep == null)
        {
            return;
        }

        _dir = (byte)(
            (Input.GetKey(KeyCode.W) ? 1 : 0)
            | (Input.GetKey(KeyCode.S) ? 2 : 0)
            | (Input.GetKey(KeyCode.A) ? 4 : 0)
            | (Input.GetKey(KeyCode.D) ? 8 : 0)
            | (Input.GetKey(KeyCode.Space) ? 16 : 0)
        );
        foreach (var kv in _fighters)
        {
            if (!_gos.TryGetValue(kv.Key, out var go))
            {
                go = Instantiate(FighterPrefab);
                _gos[kv.Key] = go;
            }
            go.transform.position = new Vector3(kv.Value[0] / 1000f, 0, kv.Value[1] / 1000f);
        }
    }

    void FixedUpdate()
    {
        Runner.Lockstep?.SendInput(new[] { _dir });
    }

    void Step(uint tick, Dictionary<ushort, byte[]> inputs)
    {
        foreach (var actor in inputs.Keys)
        {
            if (!_fighters.ContainsKey(actor))
            {
                _fighters[actor] = new[] { (int)actor * 2000, 0, 100 };
            }
        }

        foreach (var kv in inputs)
        {
            var fighter = _fighters[kv.Key];
            byte buttons = kv.Value.Length > 0 ? kv.Value[0] : (byte)0;
            if ((buttons & 1) != 0)
            {
                fighter[1] += 50;
            }

            if ((buttons & 2) != 0)
            {
                fighter[1] -= 50;
            }

            if ((buttons & 4) != 0)
            {
                fighter[0] -= 50;
            }

            if ((buttons & 8) != 0)
            {
                fighter[0] += 50;
            }

            if ((buttons & 16) != 0)
            {
                foreach (var other in _fighters)
                {
                    if (
                        other.Key != kv.Key
                        && Mathf.Abs(other.Value[0] - fighter[0])
                            + Mathf.Abs(other.Value[1] - fighter[1])
                            < 1500
                    )
                    {
                        other.Value[2] = Mathf.Max(0, other.Value[2] - 1);
                    }
                }
            }
        }
        _tick = tick + 1;
        if (tick % 10 == 0)
        {
            Runner.Lockstep.ReportHash(tick, Hash());
        }
    }

    void Restore(uint tick, ulong hash, byte[] state)
    {
        _fighters.Clear();
        _tick = tick;
        for (int offset = 0; offset + 14 <= state.Length; offset += 14)
        {
            _fighters[(ushort)((state[offset] << 8) | state[offset + 1])] = new[]
            {
                System.BitConverter.ToInt32(state, offset + 2),
                System.BitConverter.ToInt32(state, offset + 6),
                System.BitConverter.ToInt32(state, offset + 10),
            };
        }
    }

    byte[] Serialize()
    {
        var bytes = new byte[_fighters.Count * 14];
        int offset = 0;
        foreach (var kv in _fighters)
        {
            bytes[offset] = (byte)(kv.Key >> 8);
            bytes[offset + 1] = (byte)kv.Key;
            for (int i = 0; i < 3; i++)
            {
                System.BitConverter.GetBytes(kv.Value[i]).CopyTo(bytes, offset + 2 + i * 4);
            }

            offset += 14;
        }
        return bytes;
    }

    ulong Hash() => LockstepClient.Fnv1a(Serialize());
}
