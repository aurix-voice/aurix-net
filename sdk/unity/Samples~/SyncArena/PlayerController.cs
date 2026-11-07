// Sync arena sample: WASD moves your own avatar; everyone else's is interpolated by NetTransform.
// Prefab: NetView + NetTransform + PlayerController (+ a mesh). Schema: the default in NetSettings
// plus `component Player { name: string(16)  hp: u8 }`.
using Aurix.Net.Unity;
using UnityEngine;

[SyncComponent("Player")]
public sealed class PlayerController : SyncBehaviour
{
    [Sync("name")] public string PlayerName = "";
    [Sync("hp")] public byte Hp = 100;
    public float Speed = 5f;

    protected override void OnBound()
    {
        if (IsMine) { PlayerName = "player-" + View.Runner.Actor; GetComponent<Renderer>().material.color = Color.green; }
    }

    void Update()
    {
        if (!IsMine) return;
        var d = new Vector3(Input.GetAxis("Horizontal"), 0, Input.GetAxis("Vertical"));
        transform.position += d * Speed * Time.deltaTime;
        if (Input.GetKeyDown(KeyCode.Space)) View.Rpc(nameof(Wave), transform.position);
    }

    [Rpc] void Wave(Vector3 at) { Debug.Log($"{PlayerName} waves at {at}"); }

    protected override void OnSynced(uint tick) { name = PlayerName + " hp " + Hp; }
}
