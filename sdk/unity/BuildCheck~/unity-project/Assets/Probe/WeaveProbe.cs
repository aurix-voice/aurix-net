using Aurix.Net.Unity.Framework;

// A behaviour with one RPC of each kind: the editor has woven the assembly if its calls were rewritten.
public class WeaveProbe : NetworkBehaviour
{
    [ServerRpc]
    public void CmdHit(int damage, RpcInfo info = default) { }

    [ObserversRpc]
    public void RpcShout(string text, RpcInfo info = default) { }

    [TargetRpc]
    public void RpcWhisper(PlayerID to, string text) { }
}
