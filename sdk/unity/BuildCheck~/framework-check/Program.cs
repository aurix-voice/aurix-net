// Compile check + live test of the gameplay framework (NetworkManager / NetworkIdentity /
// NetworkBehaviour / SyncVar / SyncList / SyncDictionary / RPCs) over a local node. Skips the live
// part without NET_API_KEY. Scenario: host + two clients; the host spawns a player object per
// client (owner = that client) and one server object; SyncVars, an owner-auth SyncList, a
// SyncDictionary, ServerRpc / ObserversRpc (bufferLast) / TargetRpc, ownership transfer, late join,
// despawn on owner leave.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net.Unity.Framework;
using UnityEngine;

struct Loadout : INetSerializable
{
    public int weapon; public string skin;
    public void Write(Aurix.Net.Managed.BitWriter w) { NetSerializer.Write(w, weapon); NetSerializer.Write(w, skin); }
    public void Read(Aurix.Net.Managed.BitReader r) { weapon = NetSerializer.Read<int>(r); skin = NetSerializer.Read<string>(r); }
}

class Player : NetworkBehaviour
{
    public SyncVar<int> health = new SyncVar<int>(100);
    public SyncVar<string> nick = new SyncVar<string>("");
    public SyncVar<Vector3> pos = new SyncVar<Vector3>(default, 0f, ownerAuth: true);
    public SyncList<int> inventory = new SyncList<int>(ownerAuth: true);
    public SyncDictionary<string, int> stats = new SyncDictionary<string, int>();
    public SyncVar<Loadout> loadout = new SyncVar<Loadout>();
    public SyncVar<NetworkIdentity> target = new SyncVar<NetworkIdentity>();
    public readonly List<string> log = new List<string>();
    public string tag;

    [ServerRpc] void CmdHit(int dmg, RpcInfo info) { log.Add($"CmdHit({dmg}) from {info.sender}"); health.value -= dmg; }
    [ObserversRpc(bufferLast: true)] void RpcShout(string text, RpcInfo info) { log.Add($"Shout:{text}"); }
    [TargetRpc] void RpcWhisper(string text) { log.Add($"Whisper:{text}"); }
    [ServerRpc(requireOwnership: false)] void CmdAny(byte b) { log.Add($"CmdAny({b})"); }

    public void Hit(int d) => Rpc(nameof(CmdHit), d);
    public void Shout(string s) => Rpc(nameof(RpcShout), s);
    public void Whisper(PlayerID to, string s) => RpcTo(to, nameof(RpcWhisper), s);
    public void Any(byte b) => Rpc(nameof(CmdAny), b);
    protected override void OnSpawned(bool asServer) { log.Add($"spawned server={asServer} owner={owner} mine={isOwner}"); }
    protected override void OnOwnerChanged(PlayerID? o, PlayerID? n) { log.Add($"owner {o}->{n}"); }
}

class TestIdentity : NetworkIdentity { public Player player = new Player(); public TestIdentity() { behaviours.Add(player); } }

class TestFactory : INetworkObjectFactory
{
    public readonly List<TestIdentity> alive = new List<TestIdentity>();
    public NetworkIdentity Instantiate(NetworkManager m, int prefabId) { var t = new TestIdentity(); alive.Add(t); return t; }
    public void Destroy(NetworkIdentity id) => alive.Remove((TestIdentity)id);
}

static class Program
{
    static async Task<string> Grant(HttpClient h, string key, string pid)
    {
        var req = new HttpRequestMessage(HttpMethod.Post, "http://127.0.0.1:8090/v1/grants");
        req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        req.Content = new StringContent("{\"player_id\":\"" + pid + "\",\"name\":\"" + pid + "\"}", Encoding.UTF8, "application/json");
        var r = await h.SendAsync(req); var body = await r.Content.ReadAsStringAsync();
        if (!r.IsSuccessStatusCode) throw new Exception(body);
        int i = body.IndexOf("\"grant\":\"") + 9; return body.Substring(i, body.IndexOf('"', i) - i);
    }

    static async Task Pump(int ms, params NetworkManager[] ms_) { var until = DateTime.UtcNow.AddMilliseconds(ms); while (DateTime.UtcNow < until) { foreach (var m in ms_) m.NetworkUpdate(0.01f); await Task.Delay(10); } }

    static async Task Until(Func<bool> cond, int timeoutMs, params NetworkManager[] ms_) { var until = DateTime.UtcNow.AddMilliseconds(timeoutMs); while (DateTime.UtcNow < until && !cond()) { foreach (var m in ms_) m.NetworkUpdate(0.01f); await Task.Delay(10); } await Pump(150, ms_); }

    static int fails;
    static void Check(bool ok, string what) { Console.WriteLine((ok ? "  ok   " : "  FAIL ") + what); if (!ok) fails++; }
    static Player P(NetworkManager m, uint oid) => m.TryGetIdentity(oid, out var id) ? ((TestIdentity)id).player : null;

    static async Task<int> Main()
    {
        Console.WriteLine("FRAMEWORK COMPILE OK: " + string.Join(", ", new[] { typeof(NetworkManager), typeof(NetworkIdentity), typeof(NetworkBehaviour), typeof(SyncVar<>), typeof(SyncList<>), typeof(SyncDictionary<,>) }.Select(t => t.Name)));
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        if (string.IsNullOrEmpty(key)) { Console.WriteLine("NET_API_KEY unset: live test skipped"); return 0; }
        var h = new HttpClient(); var room = "fw-" + DateTime.UtcNow.Ticks;
        NetworkManager Make(string pid) { var m = new NetworkManager { roomName = room, GrantProvider = () => Grant(h, key, "fw-" + pid), factory = new TestFactory(), tickRate = 30 }; return m; }
        var host = Make("host"); var c1 = Make("c1"); var c2 = Make("c2");
        host.onError += e => Console.WriteLine("host error: " + e); c1.onError += e => Console.WriteLine("c1 error: " + e); c2.onError += e => Console.WriteLine("c2 error: " + e);

        host.StartHost(); await Until(() => host.isServer, 8000, host);
        Check(host.isServer && host.isHost && host.localPlayer.HasValue, $"host started as {host.localPlayer}");

        // a server object exists before anyone joins (late-join path for c1)
        var srvObj = (TestIdentity)host.Spawn(0);
        srvObj.player.nick.value = "server-bot"; srvObj.player.stats["kills"] = 3; srvObj.player.loadout.value = new Loadout { weapon = 7, skin = "gold" };
        srvObj.player.Shout("welcome");   // bufferLast: late joiners must get it
        await Pump(300, host);

        c1.StartClient(); await Until(() => c1.players.Count == 2 && c1.identities.Any(), 8000, host, c1);
        Check(c1.isClient && !c1.isServer && c1.players.Count == 2, $"c1 joined: players={c1.players.Count} local={c1.localPlayer}");
        var s1 = P(c1, srvObj.objectId);
        Check(s1 != null && s1.nick.value == "server-bot" && s1.stats["kills"] == 3 && s1.loadout.value.weapon == 7 && s1.loadout.value.skin == "gold", "late join: full state of server object");
        Check(s1 != null && s1.log.Contains("Shout:welcome"), "late join: buffered ObserversRpc delivered");

        // host spawns a player object owned by c1
        var p1h = (TestIdentity)host.Spawn(0, c1.localPlayer); p1h.player.nick.value = "alice"; p1h.player.target.value = srvObj;
        await Pump(400, host, c1);
        var p1c = P(c1, p1h.objectId);
        Check(p1c != null && p1c.isOwner && p1c.nick.value == "alice", "spawn with owner replicated, isOwner on c1");
        Check(p1c != null && p1c.target.value != null && p1c.target.value.objectId == srvObj.objectId, "NetworkIdentity reference resolved on client");

        // owner-auth: c1 writes pos + inventory → host sees it
        p1c.pos.value = new Vector3(1, 2, 3); p1c.inventory.Add(42); p1c.inventory.Add(43); p1c.inventory.RemoveAt(0);
        await Pump(400, host, c1);
        Check(p1h.player.pos.value.x == 1 && p1h.player.pos.value.z == 3, "ownerAuth SyncVar<Vector3> client→host");
        Check(p1h.player.inventory.Count == 1 && p1h.player.inventory[0] == 43, $"ownerAuth SyncList ops client→host ({string.Join(",", p1h.player.inventory)})");

        // non-owner-auth write on client is refused
        p1c.health.value = 1; await Pump(200, host, c1);
        Check(p1h.player.health.value == 100 && p1c.health.value == 100, "server-auth SyncVar not writable by client");

        // ServerRpc from owner, ObserversRpc back, TargetRpc
        p1c.Hit(30); await Pump(400, host, c1);
        Check(p1h.player.log.Any(l => l.StartsWith("CmdHit(30) from " + c1.localPlayer)) && p1h.player.health.value == 70 && p1c.health.value == 70, "ServerRpc with RpcInfo, state back to client");
        p1h.player.Shout("hello"); p1h.player.Whisper(c1.localPlayer.Value, "psst"); await Pump(400, host, c1);
        Check(p1c.log.Contains("Shout:hello") && p1h.player.log.Contains("Shout:hello"), "ObserversRpc on client and host");
        Check(p1c.log.Contains("Whisper:psst") && !p1h.player.log.Contains("Whisper:psst"), "TargetRpc only on target");

        // second client joins late: sees both objects, both buffered shouts, player list of 3
        c2.StartClient(); await Until(() => c2.players.Count == 3 && c2.identities.Count() == 2 && c1.players.Count == 3, 8000, host, c1, c2);
        var p1c2 = P(c2, p1h.objectId);
        Console.WriteLine($"  dbg c2 state={c2.clientState} err={c2.lastError} isClient={c2.isClient} local={c2.localPlayer} server={c2.serverPlayer} hostPlayers={host.players.Count} c2ids={c2.identities.Count()}");
        if (p1c2 == null) { Console.WriteLine("FRAMEWORK LIVE FAIL (c2 did not join)"); return 1; }
        Check(c2.players.Count == 3 && c1.players.Count == 3, $"player lists after c2 join: c1={c1.players.Count} c2={c2.players.Count}");
        Check(p1c2 != null && !p1c2.isOwner && p1c2.health.value == 70 && p1c2.pos.value.y == 2 && p1c2.inventory.Count == 1, "late joiner gets live state incl. ownerAuth modules");
        Check(p1c2 != null && p1c2.log.Contains("Shout:hello") && P(c2, srvObj.objectId).log.Contains("Shout:welcome"), "late joiner gets last buffered ObserversRpc per object");

        // non-owner ServerRpc refused on host, requireOwnership:false allowed
        p1c2.Hit(5); p1c2.Any(9); await Pump(400, host, c1, c2);
        Check(p1h.player.health.value == 70 && p1h.player.log.Contains("CmdAny(9)"), "ServerRpc ownership enforced on host; requireOwnership:false passes");

        // ownership transfer to c2
        host.SetOwnership(p1h, c2.localPlayer); await Pump(400, host, c1, c2);
        Check(p1c2.isOwner && !p1c.isOwner && p1c.log.Any(l => l.StartsWith("owner")) , "ownership transferred, OnOwnerChanged fired");
        p1c2.pos.value = new Vector3(9, 9, 9); await Pump(400, host, c1, c2);
        Check(p1h.player.pos.value.x == 9 && p1c.pos.value.x == 9, "new owner writes ownerAuth state, relayed to the old owner");

        // c2 leaves → its object is despawned everywhere
        c2.Stop(); await Until(() => host.players.Count == 2 && !c1.TryGetIdentity(p1h.objectId, out _), 8000, host, c1);
        Check(!host.TryGetIdentity(p1h.objectId, out _) && !c1.TryGetIdentity(p1h.objectId, out _) && host.players.Count == 2, "despawnOnOwnerDisconnect + player list shrink");

        // explicit despawn
        host.Despawn(srvObj); await Pump(300, host, c1);
        Check(!c1.TryGetIdentity(srvObj.objectId, out _) && ((TestFactory)c1.factory).alive.Count == 0, "explicit despawn reaches clients and destroys instances");

        host.Stop(); c1.Stop(); await Pump(300, host, c1);
        Console.WriteLine(fails == 0 ? "FRAMEWORK LIVE OK" : $"FRAMEWORK LIVE FAIL ({fails})");
        return fails == 0 ? 0 : 1;
    }
}
