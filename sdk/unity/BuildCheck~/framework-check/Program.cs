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
    public SyncHashSet<string> tags = new SyncHashSet<string>();
    public SyncQueue<int> queue = new SyncQueue<int>();
    public SyncArray<int> slots = new SyncArray<int>(4);
    public SyncEvent<int> boom = new SyncEvent<int>();
    public SyncTimer timer = new SyncTimer();
    public int boomsSeen;
    public SyncVar<int> health = new SyncVar<int>(100);
    public SyncVar<string> nick = new SyncVar<string>("");
    public SyncVar<Vector3> pos = new SyncVar<Vector3>(default, 0f, ownerAuth: true);
    public SyncList<int> inventory = new SyncList<int>(ownerAuth: true);
    public SyncDictionary<string, int> stats = new SyncDictionary<string, int>();
    public SyncVar<Loadout> loadout = new SyncVar<Loadout>();
    public SyncVar<NetworkIdentity> target = new SyncVar<NetworkIdentity>();
    public readonly List<string> log = new List<string>();
    public string tag;

    [ServerRpc] void CmdHit(int dmg, RpcInfo info = default) { log.Add($"CmdHit({dmg}) from {info.sender}"); health.value -= dmg; }
    [ObserversRpc(bufferLast: true)] void RpcShout(string text, RpcInfo info = default) { log.Add($"Shout:{text}"); }
    [TargetRpc] void RpcWhisper(PlayerID to, string text) { log.Add($"Whisper:{text}"); }
    [ServerRpc(requireOwnership: false)] void CmdAny(byte b) { log.Add($"CmdAny({b})"); }

    public void Hit(int d) => CmdHit(d);                       // woven: direct call
    public void Shout(string s) => Rpc(nameof(RpcShout), s);   // reflection mode still works on woven code
    public void Whisper(PlayerID to, string s) => RpcWhisper(to, s);
    public void Any(byte b) => CmdAny(b);
    public static bool Woven => typeof(Player).GetMethod("CmdHit" + ImplSuffix, System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic) != null;
    protected override void OnSpawned(bool asServer) { log.Add($"spawned server={asServer} owner={owner} mine={isOwner}"); }
    protected override void OnOwnerChanged(PlayerID? o, PlayerID? n) { log.Add($"owner {o}->{n}"); }
}

struct MoveInput : INetSerializable { public float dx; public void Write(Aurix.Net.Managed.BitWriter w) => w.WriteF32(dx); public void Read(Aurix.Net.Managed.BitReader r) => dx = r.ReadF32(); }
struct MoveState : INetSerializable, IEquatable<MoveState>
{
    public float x; public int steps;
    public void Write(Aurix.Net.Managed.BitWriter w) { w.WriteF32(x); w.WriteBits((uint)steps, 32); }
    public void Read(Aurix.Net.Managed.BitReader r) { x = r.ReadF32(); steps = (int)(uint)r.ReadBits(32); }
    public bool Equals(MoveState o) => Math.Abs(x - o.x) < 1e-4f && steps == o.steps;
    public override bool Equals(object o) => o is MoveState m && Equals(m);
    public override int GetHashCode() => steps;
}
class Pawn : PredictedBehaviour<MoveInput, MoveState>
{
    public float dx; public float impulse; public int impulsesApplied; public float viewX;
    protected override void GetInput(ref MoveInput i) => i.dx = dx;
    protected override void Simulate(MoveInput i, ref MoveState s, float dt) { s.x += i.dx * 10f * dt; s.steps++; }
    protected override void ServerModifyState(ref MoveState s) { if (impulse != 0) { s.x += impulse; impulse = 0; impulsesApplied++; } }
    protected override MoveState Interpolate(MoveState a, MoveState b, float t) => new MoveState { x = a.x + (b.x - a.x) * t, steps = b.steps };
    protected override void UpdateView(MoveState v, MoveState? ver) => viewX = v.x;
}
class PawnIdentity : NetworkIdentity { public Pawn pawn = new Pawn(); public PawnIdentity() { behaviours.Add(pawn); } }
class TestIdentity : NetworkIdentity { public Player player = new Player(); public TestIdentity() { behaviours.Add(player); } }
static class Chat
{
    public static readonly List<string> serverLog = new List<string>(), clientLog = new List<string>(), whispers = new List<string>();
    [ServerRpc] public static void Say(string text, RpcInfo info = default) { serverLog.Add(info.sender + ":" + text); }
    [ObserversRpc] public static void Announce(string text) { clientLog.Add(text); }
    [TargetRpc] public static void Whisper(PlayerID to, string text) { whispers.Add(to + "<" + text); }
    [ObserversRpc(bufferLast: true)] public static void Motd(string text) { clientLog.Add("motd:" + text); }
}
class Target : NetworkIdentity { public ColliderRollback cr = new ColliderRollback(); public Target() { behaviours.Add(cr); } }
class Idle : StateNode { public int enters, exits; public override void Enter() => enters++; public override void Exit() => exits++; }
class Round : StateNode<int> { public int enters, exits, last; public override void Enter(int n) { enters++; last = n; } public override void Exit() => exits++; }
class Phases : NetworkIdentity { public StateMachine sm = new StateMachine(); public Idle idle = new Idle(); public Round round = new Round(); public Phases() { sm.states.Add(idle); sm.states.Add(round); behaviours.Add(sm); behaviours.Add(idle); behaviours.Add(round); } }
class Counter : NetworkBehaviour { public SyncVar<int> v = new SyncVar<int>(); }
class LodBox : NetworkIdentity { public NetworkLOD lod = new NetworkLOD { evaluateIntervalTicks = 1 }; public Counter counter = new Counter(); public LodBox() { behaviours.Add(lod); behaviours.Add(counter); } }
class Mover : NetworkIdentity { public NetworkTransform nt = new NetworkTransform { syncParent = true }; public Mover() { behaviours.Add(nt); } }

class TestFactory : INetworkObjectFactory
{
    public readonly List<TestIdentity> alive = new List<TestIdentity>();
    public readonly List<NetworkIdentity> all = new List<NetworkIdentity>();
    public NetworkIdentity Instantiate(NetworkManager m, int prefabId) { if (prefabId == 4) { var ph = new Phases(); all.Add(ph); return ph; } if (prefabId == 5) { var lb = new LodBox(); all.Add(lb); return lb; } if (prefabId == 3) { var tg = new Target(); all.Add(tg); return tg; } if (prefabId == 2) { var pw = new PawnIdentity(); all.Add(pw); return pw; } if (prefabId == 1) { var mv = new Mover(); all.Add(mv); return mv; } var t = new TestIdentity(); alive.Add(t); all.Add(t); return t; }
    public void Destroy(NetworkIdentity id) { all.Remove(id); if (id is TestIdentity t) alive.Remove(t); }
}

static class Program
{
    const int PH = 4, LB = 5;
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
        Console.WriteLine("RPC methods woven: " + Player.Woven);
        Console.WriteLine("FRAMEWORK COMPILE OK: " + string.Join(", ", new[] { typeof(NetworkManager), typeof(NetworkIdentity), typeof(NetworkBehaviour), typeof(SyncVar<>), typeof(SyncList<>), typeof(SyncDictionary<,>) }.Select(t => t.Name)));
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        if (string.IsNullOrEmpty(key)) { Console.WriteLine("NET_API_KEY unset: live test skipped"); return 0; }
        var h = new HttpClient(); var room = "fw-" + DateTime.UtcNow.Ticks;
        NetworkManager Make(string pid) { var m = new NetworkManager { roomName = room, GrantProvider = () => Grant(h, key, "fw-" + pid), factory = new TestFactory(), tickRate = 30 }; return m; }
        var host = Make("host"); var c1 = Make("c1"); var c2 = Make("c2");
        host.onError += e => Console.WriteLine("host error: " + e); c1.onError += e => Console.WriteLine("c1 error: " + e); c2.onError += e => Console.WriteLine("c2 error: " + e);

        host.StartHost(); await Until(() => host.isServer, 8000, host);
        Check(host.isServer && host.isHost && host.localPlayer.HasValue, $"host started as {host.localPlayer}");
        Check(Player.Woven, "test assembly was woven (CmdHit__Impl present)");

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

        // NetworkTransform: owner moves 1 unit per tick along x; remote interpolates behind and follows
        var mvH = (Mover)host.Spawn(1, c1.localPlayer); await Until(() => c1.TryGetIdentity(mvH.objectId, out _), 5000, host, c1);
        var mvC = (Mover)(c1.TryGetIdentity(mvH.objectId, out var mvId) ? mvId : null);
        Check(mvC != null && mvC.nt.isOwner, "NetworkTransform object spawned, owner = c1");
        async Task PumpMover(int ms) { var until = DateTime.UtcNow.AddMilliseconds(ms); while (DateTime.UtcNow < until) { host.NetworkUpdate(0.01f); c1.NetworkUpdate(0.01f); mvH.nt.RenderUpdate(0.01f); mvC.nt.RenderUpdate(0.01f); await Task.Delay(10); } }
        for (int i = 1; i <= 20; i++) { mvC.nt.transform.position = new Vector3(i, 0, 0); mvC.nt.transform.rotation = Quaternion.Euler(0, i * 9, 0); await PumpMover(34); }
        await PumpMover(400);
        float hx = mvH.nt.transform.position.x; float ang = Quaternion.Angle(mvH.nt.transform.rotation, Quaternion.Euler(0, hx * 9, 0)); // rotation must match the interpolated position
        Check(hx >= 15 && hx <= 20.01f && ang < 2f, $"remote follows owner motion with interpolation (host x={hx:0.##}, rot err={ang:0.##}°)");
        Check(mvH.nt.sync.buffer.Count >= 2 && mvH.nt.sync.buffer[mvH.nt.sync.buffer.Count - 1].tick > mvH.nt.sync.buffer[0].tick, "samples stamped with increasing server ticks");
        mvC.nt.Teleport(new Vector3(100, 0, 0), Quaternion.identity); await PumpMover(400);
        Check(Math.Abs(mvH.nt.transform.position.x - 100) < 0.01f, $"teleport snaps on remote (x={mvH.nt.transform.position.x:0.##})");

        // extra sync modules: hash set, queue, array, event, timer (server → client, plus late join)
        {
            var hostX = Make("hostX"); var cX = Make("cX"); var roomX = room + "-mods"; hostX.roomName = cX.roomName = roomX;
            hostX.StartHost(); await Until(() => hostX.isServer, 8000, hostX);
            var px = (TestIdentity)hostX.Spawn(0);
            px.player.tags.Add("vip"); px.player.tags.Add("beta"); px.player.queue.Enqueue(1); px.player.queue.Enqueue(2); px.player.queue.Enqueue(3); px.player.slots[2] = 42; px.player.timer.Start(1.2f);
            cX.StartClient(); await Until(() => cX.TryGetIdentity(px.objectId, out _), 8000, hostX, cX);
            var pc = ((TestIdentity)(cX.TryGetIdentity(px.objectId, out var pxi) ? pxi : null)).player;
            pc.boom.onInvoked += v => pc.boomsSeen += v;
            await Pump(300, hostX, cX);
            Check(pc.tags.Contains("vip") && pc.tags.Count == 2 && pc.queue.Count == 3 && pc.queue.Peek() == 1 && pc.slots[2] == 42 && pc.timer.isRunning, $"late joiner gets full hash set/queue/array/timer state (timer remaining {pc.timer.remaining:0.00}s)");
            px.player.tags.Remove("beta"); px.player.queue.Dequeue(); px.player.slots[0] = 7; px.player.boom.Invoke(5); px.player.boom.Invoke(6);
            await Pump(400, hostX, cX);
            Check(!pc.tags.Contains("beta") && pc.tags.Count == 1 && pc.queue.Count == 2 && pc.queue.Peek() == 2 && pc.slots[0] == 7 && pc.slots[2] == 42 && pc.boomsSeen == 11, $"deltas: remove/dequeue/slot/event replicated (booms {pc.boomsSeen})");
            Check(!pc.timer.finished && pc.timer.remaining > 0.2f, $"SyncTimer still running on the client ({pc.timer.remaining:0.00}s left)");
            await Until(() => pc.timer.finished && px.player.timer.finished, 3000, hostX, cX);
            Check(pc.timer.finished && px.player.timer.finished, "SyncTimer finished on both sides from the server tick");
            hostX.Stop(); cX.Stop(); await Pump(300, hostX, cX);
        }

        // NetworkTransform parent sync: child attached to a networked parent replicates the parent + local pose
        {
            var hostP = Make("hostP"); var cP = Make("cP"); hostP.roomName = cP.roomName = room + "-parent";
            hostP.StartHost(); await Until(() => hostP.isServer, 8000, hostP);
            var pa = (Mover)hostP.Spawn(1); var ch = (Mover)hostP.Spawn(1);
            pa.nt.transform.position = new Vector3(10, 0, 0);
            ch.nt.SetParent(pa); ch.nt.transform.localPosition = new Vector3(1, 2, 3);
            cP.StartClient(); await Until(() => cP.TryGetIdentity(ch.objectId, out _) && cP.TryGetIdentity(pa.objectId, out _), 8000, hostP, cP);
            await Pump(400, hostP, cP);
            cP.TryGetIdentity(pa.objectId, out var cpa); cP.TryGetIdentity(ch.objectId, out var cch);
            var cnt = ((Mover)cch).nt;
            Check(cnt.transform.parent == cpa.transform && cnt.parentIdentity == cpa && Vector3.Distance(cnt.transform.localPosition, new Vector3(1, 2, 3)) < 0.01f, $"late joiner re-parents the child under the networked parent with the local pose ({cnt.transform.localPosition.x},{cnt.transform.localPosition.y},{cnt.transform.localPosition.z})");
            ch.nt.SetParent(null); ch.nt.transform.position = new Vector3(5, 5, 5);
            for (int i = 0; i < 40; i++) { hostP.NetworkUpdate(0.01f); cP.NetworkUpdate(0.01f); cnt.RenderUpdate(0.01f); await Task.Delay(10); }
            Check(cnt.transform.parent == null && cnt.parentIdentity == null && Vector3.Distance(cnt.transform.position, new Vector3(5, 5, 5)) < 0.01f, "detach replicates: parent cleared, world pose follows");
            hostP.Stop(); cP.Stop(); await Pump(300, hostP, cP);
        }

        // replicated StateMachine: controller switches states (with payload), peers run Enter/Exit, late joiner enters the current state
        {
            var hostM = Make("hostM"); var cM = Make("cM"); var cM2 = Make("cM2"); hostM.roomName = cM.roomName = cM2.roomName = room + "-sm";
            hostM.StartHost(); await Until(() => hostM.isServer, 8000, hostM);
            var ph = (Phases)hostM.Spawn(PH);
            cM.StartClient(); await Until(() => cM.TryGetIdentity(ph.objectId, out _), 8000, hostM, cM);
            Check(ph.sm.SetState(ph.idle) && ph.idle.enters == 1, "server enters Idle locally");
            await Until(() => cM.TryGetIdentity(ph.objectId, out var x) && ((Phases)x).idle.enters == 1, 3000, hostM, cM);
            cM.TryGetIdentity(ph.objectId, out var phC1); var phC = (Phases)phC1;
            Check(phC.idle.enters == 1 && phC.sm.IsCurrentState<Idle>(), "client entered Idle");
            Check(ph.sm.SetState(ph.round, 7), "server switches to Round(7)");
            await Until(() => phC.round.enters == 1, 3000, hostM, cM);
            Check(phC.round.last == 7 && phC.idle.exits == 1 && phC.sm.currentStateNode == phC.round && phC.sm.previousStateNode == phC.idle, $"client exited Idle and entered Round with payload {phC.round.last}");
            cM2.StartClient(); await Until(() => cM2.TryGetIdentity(ph.objectId, out _), 8000, hostM, cM, cM2); await Pump(200, hostM, cM, cM2);
            cM2.TryGetIdentity(ph.objectId, out var phC2); var late = (Phases)phC2;
            Check(late.round.enters == 1 && late.round.last == 7 && late.idle.enters == 0, "late joiner enters only the current state Round(7)");
            Check(ph.sm.Next() && ph.sm.IsCurrentState<Idle>(), "Next() wraps to Idle");
            await Until(() => phC.idle.enters == 2 && late.idle.enters == 1, 3000, hostM, cM, cM2);
            Check(phC.round.exits == 1 && late.round.exits == 1, "Round exited on both clients");
            hostM.Stop(); cM.Stop(); cM2.Stop(); await Pump(300, hostM, cM, cM2);
        }

        // NetworkLOD: far player is culled from deltas (still an observer), near player gets them; moving closer restores
        {
            var hostL = Make("hostL"); var cN = Make("cN"); var cF = Make("cF"); hostL.roomName = cN.roomName = cF.roomName = room + "-lod";
            hostL.StartHost(); await Until(() => hostL.isServer, 8000, hostL);
            var box = (LodBox)hostL.Spawn(LB);
            cN.StartClient(); cF.StartClient(); await Until(() => hostL.players.Count == 3, 8000, hostL, cN, cF);
            var avN = (Mover)hostL.Spawn(1, cN.localPlayer); var avF = (Mover)hostL.Spawn(1, cF.localPlayer);
            avN.transform.position = new Vector3(5, 0, 0); avF.transform.position = new Vector3(600, 0, 0);
            await Until(() => cN.TryGetIdentity(box.objectId, out _) && cF.TryGetIdentity(box.objectId, out _), 5000, hostL, cN, cF);
            cN.TryGetIdentity(box.objectId, out var bN); cF.TryGetIdentity(box.objectId, out var bF);
            await Pump(300, hostL, cN, cF);
            Check(box.lod.GetTier(cN.localPlayer.Value) == 0 && box.lod.IsCulled(cF.localPlayer.Value), $"tiers: near={box.lod.GetTier(cN.localPlayer.Value)} far culled");
            int before = ((LodBox)bF).counter.v.value;
            for (int i = 0; i < 20; i++) { box.counter.v.value++; await Pump(30, hostL, cN, cF); }
            Check(((LodBox)bN).counter.v.value == box.counter.v.value && ((LodBox)bF).counter.v.value == before && box.IsObserver(cF.localPlayer.Value), $"near client follows ({((LodBox)bN).counter.v.value}), culled client frozen at {before} but still observes");
            avF.transform.position = new Vector3(8, 0, 0); await Pump(300, hostL, cN, cF);
            box.counter.v.value++; await Until(() => ((LodBox)bF).counter.v.value == box.counter.v.value, 3000, hostL, cN, cF);
            Check(box.lod.GetTier(cF.localPlayer.Value) == 0 && ((LodBox)bF).counter.v.value == box.counter.v.value, "far player moved close: tier 0 and deltas resume");
            hostL.Stop(); cN.Stop(); cF.Stop(); await Pump(300, hostL, cN, cF);
        }

        // static RPCs: no NetworkIdentity, routed via NetworkManager.main (host) / the client manager
        {
            // NetworkManager.main is whichever manager started last in this process; make the client "main" to send, then the host to receive
            Check(typeof(Chat).GetMethod("Say__Impl", System.Reflection.BindingFlags.Static | System.Reflection.BindingFlags.NonPublic) != null, "static RPC woven (Say__Impl present)");
            var hostS = Make("hostS"); var cS = Make("cS"); hostS.roomName = cS.roomName = room + "-static";
            hostS.StartHost(); await Until(() => hostS.isServer, 8000, hostS);
            NetworkManager.SetMain(hostS);   // several managers live in this process; a Unity game has one
            Chat.Motd("welcome"); Chat.clientLog.Clear();  // buffered before the client exists
            cS.StartClient(); await Until(() => cS.isClient && hostS.players.Count == 2, 8000, hostS, cS);
            await Pump(300, hostS, cS);
            Check(Chat.clientLog.Contains("motd:welcome"), "late joiner gets bufferLast static ObserversRpc");
            NetworkManager.SetMain(cS); Chat.Say("hi from client");
            await Until(() => Chat.serverLog.Count > 0, 3000, hostS, cS);
            Check(Chat.serverLog.Count == 1 && Chat.serverLog[0] == cS.localPlayer + ":hi from client", $"static ServerRpc reached the host with the sender ({string.Join(",", Chat.serverLog)})");
            NetworkManager.SetMain(hostS);
            Chat.Announce("round 2"); Chat.Whisper(cS.localPlayer.Value, "psst"); Chat.Whisper(hostS.localPlayer.Value, "self");
            await Until(() => Chat.clientLog.Contains("round 2") && Chat.whispers.Count >= 2, 3000, hostS, cS);
            Check(Chat.clientLog.Count(x => x == "round 2") == 2 && Chat.whispers.Contains(cS.localPlayer + "<psst") && Chat.whispers.Contains(hostS.localPlayer + "<self") && Chat.whispers.Count == 2,
                $"static ObserversRpc ran on host+client, static TargetRpc on the target only (whispers {string.Join(",", Chat.whispers)})");
            hostS.Stop(); cS.Stop(); await Pump(300, hostS, cS);
        }

        // Lag compensation: server rewinds ColliderRollback poses to the shooter's view tick
        {
            var hostL = Make("hostL"); var cL = Make("cL"); hostL.roomName = cL.roomName = room + "-lag";
            hostL.StartHost(); await Until(() => hostL.isServer, 8000, hostL);
            var tg = (Target)hostL.Spawn(3);
            cL.StartClient(); await Until(() => cL.TryGetIdentity(tg.objectId, out _), 8000, hostL, cL);
            // the server moves the target one unit per tick (x = server tick)
            var mover = new Func<Task>(async () => { var until = DateTime.UtcNow.AddMilliseconds(600); while (DateTime.UtcNow < until) { tg.cr.transform.position = new Vector3(hostL.serverTick, 0, 0); hostL.NetworkUpdate(0.01f); cL.NetworkUpdate(0.01f); await Task.Delay(5); } });
            await mover();
            uint view = hostL.ViewTickOf(cL.localPlayer.Value); float seenX = -1, liveX = tg.cr.transform.position.x;
            hostL.Rollback(cL.localPlayer.Value, () => seenX = tg.cr.transform.position.x);
            tg.cr.TryGetPose(view, out var hp);
            Check(view > 0 && view < hostL.serverTick && view >= hostL.serverTick - 6 && Math.Abs(seenX - hp.pos.x) < 0.001f && seenX < liveX - 1 && Math.Abs(tg.cr.transform.position.x - liveX) < 0.001f,
                $"Rollback(shooter) sees the recorded pose at the shooter's view tick (view {view}, server {hostL.serverTick}, seen x={seenX:0.##}, live x={liveX:0.##}) and restores");
            Check(tg.cr.history.Count > 10 && !tg.cr.isRolledBack, $"server keeps a pose history ({tg.cr.history.Count} ticks)");
            hostL.Stop(); cL.Stop(); await Pump(300, hostL, cL);
        }

        // Quick match: StartAuto hosts when nothing matches, the next StartAuto joins that room as a client
        {
            var qa = Make("qa"); var qb = Make("qb"); var tag = "{\"m\":\"" + room + "-q\"}";
            qa.matchFilterJson = qb.matchFilterJson = tag; qa.roomPropsJson = qb.roomPropsJson = tag;
            qa.StartAuto(); await Until(() => qa.isServer, 8000, qa);
            qb.StartAuto(); await Until(() => qb.isClient && qb.players.Count == 2 && qa.players.Count == 2, 8000, qa, qb);
            Check(qa.isHost && !qb.isServer && qb.serverPlayer == qa.localPlayer, $"StartAuto: first became host, second joined as client (server player {qb.serverPlayer})");
            var q = (TestIdentity)qa.Spawn(0, qb.localPlayer); await Until(() => qb.identities.Any(i => i.isOwner), 5000, qa, qb);
            Check(qb.identities.Any(i => i.isOwner), "quick-matched client receives spawns");
            qa.Stop(); qb.Stop(); await Pump(300, qa, qb);
        }

        // Host migration: host leaves → the node promotes a client; it becomes server with the world it holds, the other client re-attaches
        {
            var hostM = Make("hostM"); var a = Make("aM"); var b = Make("bM"); var roomM = room + "-mig"; hostM.roomName = a.roomName = b.roomName = roomM;
            hostM.StartHost(); await Until(() => hostM.isServer, 8000, hostM);
            a.StartClient(); b.StartClient(); await Until(() => a.players.Count == 3 && b.players.Count == 3, 8000, hostM, a, b);
            var objA = (TestIdentity)hostM.Spawn(0, a.localPlayer); objA.player.nick.value = "A"; var objS = (TestIdentity)hostM.Spawn(0); objS.player.nick.value = "S";
            await Until(() => a.identities.Count() == 2 && b.identities.Count() == 2, 5000, hostM, a, b);
            hostM.Stop(); await Until(() => a.isServer || b.isServer, 10000, a, b);
            var srv = a.isServer ? a : b; var cli = a.isServer ? b : a;
            await Until(() => cli.serverPlayer == srv.localPlayer && cli.players.Count == 2 && srv.players.Count == 2, 8000, a, b);
            Check(srv.isServer && !cli.isServer && cli.serverPlayer == srv.localPlayer, $"new master promoted to server ({srv.localPlayer}); other client re-attached");
            Check(srv.identities.Count() == 2 && cli.identities.Count() == 2 && ((TestIdentity)(srv.TryGetIdentity(objS.objectId, out var ss) ? ss : null))?.player.nick.value == "S", "world survived the migration with the same object ids");
            var fresh = (TestIdentity)srv.Spawn(0); fresh.player.nick.value = "after"; await Until(() => cli.TryGetIdentity(fresh.objectId, out _), 5000, a, b); await Pump(300, a, b);
            Check(fresh.objectId > objS.objectId && ((TestIdentity)(cli.TryGetIdentity(fresh.objectId, out var fc) ? fc : null))?.player.nick.value == "after", "new host spawns and replicates after migration");
            a.Stop(); b.Stop(); await Pump(300, a, b);
        }

        // Prediction: owner predicts, server verifies, observer interpolates; a server impulse forces a reconciliation
        {
            var hostP = Make("hostP"); var cP = Make("cP"); var oP = Make("oP"); var roomP = room + "-pred"; hostP.roomName = cP.roomName = oP.roomName = roomP;
            hostP.StartHost(); await Until(() => hostP.isServer, 8000, hostP);
            cP.StartClient(); oP.StartClient(); await Until(() => cP.players.Count == 3 && oP.players.Count == 3, 8000, hostP, cP, oP);
            var pawnH = (PawnIdentity)hostP.Spawn(2, cP.localPlayer);
            await Until(() => cP.TryGetIdentity(pawnH.objectId, out _) && oP.TryGetIdentity(pawnH.objectId, out _), 5000, hostP, cP, oP);
            var pawnC = (PawnIdentity)(cP.TryGetIdentity(pawnH.objectId, out var pi) ? pi : null); var pawnO = (PawnIdentity)(oP.TryGetIdentity(pawnH.objectId, out var po) ? po : null);
            async Task PumpP(int ms) { var until = DateTime.UtcNow.AddMilliseconds(ms); while (DateTime.UtcNow < until) { hostP.NetworkUpdate(0.01f); cP.NetworkUpdate(0.01f); oP.NetworkUpdate(0.01f); pawnO.pawn.RenderUpdate(0.01f); await Task.Delay(10); } }
            await PumpP(300);
            pawnC.pawn.dx = 1f; await PumpP(120); float early = pawnC.pawn.state.x;
            Check(early > 0.2f, $"owner predicts immediately (x={early:0.##} after 120 ms)");
            await PumpP(900); pawnC.pawn.dx = 0f; await PumpP(600);
            float sx = pawnH.pawn.state.x, cx = pawnC.pawn.state.x;
            Check(sx > 5f && Math.Abs(sx - cx) < 0.05f, $"server and owner converge after input stops (server x={sx:0.##}, owner x={cx:0.##}, missed inputs={pawnH.pawn.missedInputs}, reconciliations={pawnC.pawn.reconciliations})");
            Check(Math.Abs(pawnO.pawn.viewX - sx) < 0.05f, $"observer view follows verified state (view x={pawnO.pawn.viewX:0.##})");
            int before = pawnC.pawn.reconciliations; pawnH.pawn.impulse = 10f; await PumpP(600);
            Check(pawnH.pawn.impulsesApplied == 1 && pawnC.pawn.reconciliations > before && Math.Abs(pawnC.pawn.state.x - pawnH.pawn.state.x) < 0.05f, $"server impulse → owner reconciles to x={pawnC.pawn.state.x:0.##} (server {pawnH.pawn.state.x:0.##}, reconciliations {pawnC.pawn.reconciliations})");
            hostP.Stop(); cP.Stop(); oP.Stop(); await Pump(300, hostP, cP, oP);
        }

        // PlayerSpawner: one Mover per player (host included), owned by that player, gone when they leave
        {
            var hostS = Make("hostS"); var cS = Make("cS"); var roomS = room + "-spawner"; hostS.roomName = roomS; cS.roomName = roomS;
            var spawner = new PlayerSpawner { manager = hostS, prefabId = 1 }; spawner.Attach(hostS);
            hostS.StartHost(); await Until(() => hostS.isServer && spawner.spawned.Count == 1, 8000, hostS);
            cS.StartClient(); await Until(() => cS.identities.Count() == 2, 8000, hostS, cS);
            var mine = cS.identities.FirstOrDefault(i => i.isOwner);
            Check(spawner.spawned.Count == 2 && mine != null && spawner.spawned[cS.localPlayer.Value].objectId == mine.objectId, "PlayerSpawner spawned one owned object per player (host + client)");
            cS.Stop(); await Until(() => hostS.identities.Count() == 1, 8000, hostS);
            Check(spawner.spawned.Count == 1 && hostS.identities.Count() == 1, "PlayerSpawner: object despawned and forgotten when the player left");
            hostS.Stop(); await Pump(300, hostS);
        }

        // interest management: DistanceRule — c1's avatar at x=100 sees a server object at x=110 but not one at x=500
        {
            var hostV = Make("hostV"); var cV = Make("cV"); var roomV = room + "-vis"; hostV.roomName = roomV; cV.roomName = roomV;
            hostV.visibilityRules.Add(new DistanceRule(hostV, 50f)); hostV.visibilityIntervalTicks = 2;
            hostV.StartHost(); await Until(() => hostV.isServer, 8000, hostV);
            var near = (Mover)hostV.Spawn(1); near.nt.transform.position = new Vector3(110, 0, 0); near.nt.RenderUpdate(0);
            var far = (Mover)hostV.Spawn(1); far.nt.transform.position = new Vector3(500, 0, 0); far.nt.RenderUpdate(0);
            cV.StartClient(); await Until(() => cV.players.Count == 2, 8000, hostV, cV);
            var avatar = (Mover)hostV.Spawn(1, cV.localPlayer);
            await Until(() => cV.TryGetIdentity(avatar.objectId, out _), 5000, hostV, cV);
            var avC = (Mover)(cV.TryGetIdentity(avatar.objectId, out var avId) ? avId : null);
            avC.nt.transform.position = new Vector3(100, 0, 0);
            async Task PumpV(int ms) { var until = DateTime.UtcNow.AddMilliseconds(ms); while (DateTime.UtcNow < until) { hostV.NetworkUpdate(0.01f); cV.NetworkUpdate(0.01f); avC.nt.RenderUpdate(0.01f); avatar.nt.RenderUpdate(0.01f); await Task.Delay(10); } }
            await PumpV(600);
            Check(cV.TryGetIdentity(near.objectId, out _) && !cV.TryGetIdentity(far.objectId, out _) && near.IsObserver(cV.localPlayer.Value) && !far.IsObserver(cV.localPlayer.Value), $"DistanceRule: near visible, far hidden (client has {cV.identities.Count()} objects)");
            avC.nt.transform.position = new Vector3(480, 0, 0); await PumpV(600);
            Check(cV.TryGetIdentity(far.objectId, out _) && !cV.TryGetIdentity(near.objectId, out _), "moving the avatar flips visibility: far spawned, near despawned on the client");
            hostV.Stop(); cV.Stop(); await Pump(300, hostV, cV);
        }

        // scene object: same sceneId registered on every peer, bound (not instantiated) and replicated
        host.Stop(); c1.Stop(); await Pump(300, host, c1);
        var room2 = room + "-scene";
        var host2 = Make("host2"); var c3 = Make("c3"); host2.roomName = room2; c3.roomName = room2;
        var soH = new TestIdentity { sceneId = 77 }; var soC = new TestIdentity { sceneId = 77 };
        host2.RegisterSceneObject(soH); c3.RegisterSceneObject(soC);
        host2.StartHost(); await Until(() => host2.isServer && soH.isSpawned, 8000, host2);
        soH.player.nick.value = "door"; soH.player.stats["open"] = 1;
        c3.StartClient(); await Until(() => soC.isSpawned, 8000, host2, c3);
        await Pump(300, host2, c3);
        Check(soC.isSpawned && soC.objectId == soH.objectId && soC.player.nick.value == "door" && soC.player.stats["open"] == 1 && ((TestFactory)c3.factory).all.Count == 0, "scene object bound by sceneId with state, nothing instantiated");
        host2.Stop(); c3.Stop(); await Pump(300, host2, c3);
        Check(!soC.isSpawned && !soH.isSpawned, "scene objects survive despawn (not destroyed)");
        Console.WriteLine(fails == 0 ? "FRAMEWORK LIVE OK" : $"FRAMEWORK LIVE FAIL ({fails})");
        return fails == 0 ? 0 : 1;
    }
}
