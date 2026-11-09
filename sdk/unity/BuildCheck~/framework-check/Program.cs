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
class Plain { public int hp = 5; public string title { get; set; } = "start"; public Vector3 pos; public int healed; public string lastWhisper; public void Heal(int n) { hp += n; healed++; } public void Whisper(string s) => lastWhisper = s; }
class Reflected : NetworkIdentity { public Plain plain = new Plain(); public NetworkReflection refl; public Reflected() { refl = new NetworkReflection { trackedBehaviour = plain, trackedFields = { "hp", "title", "pos" }, trackedMethods = { new ReflectedMethod("Heal", ReflectionRpcType.Server), new ReflectedMethod("Whisper", ReflectionRpcType.Target) } }; behaviours.Add(refl); } }
class Rig : NetworkIdentity { public Transform[] b = { new Transform(), new Transform(), new Transform() }; public NetworkBones bones; public Rig() { bones = new NetworkBones { extraBones = b, sendRate = 30, smoothing = 0 }; behaviours.Add(bones); } }
class Speaker : NetworkIdentity { public AudioSource src = new AudioSource(); public NetworkAudioSource audio; public Speaker() { audio = new NetworkAudioSource { audioSource = src, clips = { new AudioClip { name = "a" }, new AudioClip { name = "b" } } }; behaviours.Add(audio); } }
class Racer : NetworkBehaviour { public ValidatedSyncVar<int> speed = new ValidatedSyncVar<int>(1, 0f, (cur, proposed) => proposed <= 10); public SyncBigData blob = new SyncBigData(false, 4096); }
class Inputs : NetworkBehaviour { public SyncInput<int> ability = new SyncInput<int>(); public SyncTexture tex = new SyncTexture(); public SyncFile file = new SyncFile(); }
class InputsIdentity : NetworkIdentity { public Inputs inputs = new Inputs(); public InputsIdentity() { behaviours.Add(inputs); } }
class RacerIdentity : NetworkIdentity { public Racer racer = new Racer(); public RacerIdentity() { behaviours.Add(racer); } }
class Packer : NetworkBehaviour
{
    public List<string> bigs = new List<string>(); public List<(Vector3, int, string)> states = new List<(Vector3, int, string)>();
    [ObserversRpc(compression: RpcCompression.Fast)] public void Big(string text) => bigs.Add(text);
    [ObserversRpc(deltaPack: true)] public void State(Vector3 pos, int hp, string name) => states.Add((pos, hp, name));
}
class Generic : NetworkBehaviour
{
    public List<object> seen = new List<object>(); public List<string> pushed = new List<string>();
    [ObserversRpc] public void Show<T>(T v) => seen.Add(v);
    [ServerRpc(requireOwnership: false)] public void Push<T>(T v, RpcInfo info = default) => pushed.Add(info.sender + ":" + v);
}
class GenericIdentity : NetworkIdentity { public Generic g = new Generic(); public GenericIdentity() { behaviours.Add(g); } }
class PackerIdentity : NetworkIdentity { public Packer packer = new Packer(); public PackerIdentity() { behaviours.Add(packer); } }
class DelayedFactory : IAsyncNetworkObjectFactory
{
    public INetworkObjectFactory inner; public int delayPumps = 20; readonly List<(int left, int pid, Action<NetworkIdentity> ready)> _pending = new List<(int, int, Action<NetworkIdentity>)>();
    public NetworkIdentity Instantiate(NetworkManager m, int prefabId) => inner.Instantiate(m, prefabId);
    public void Destroy(NetworkIdentity id) => inner.Destroy(id);
    public void InstantiateAsync(NetworkManager m, int prefabId, Action<NetworkIdentity> ready) => _pending.Add((delayPumps, prefabId, ready));
    public void Pump(NetworkManager m) { for (int i = _pending.Count - 1; i >= 0; i--) { var p = _pending[i]; if (--p.left <= 0) { _pending.RemoveAt(i); p.ready(inner.Instantiate(m, p.pid)); } else _pending[i] = p; } }
}
class Mover : NetworkIdentity { public NetworkTransform nt = new NetworkTransform { syncParent = true }; public Mover() { behaviours.Add(nt); } }

class TestFactory : INetworkObjectFactory
{
    public readonly List<TestIdentity> alive = new List<TestIdentity>();
    public readonly List<NetworkIdentity> all = new List<NetworkIdentity>();
    public NetworkIdentity Instantiate(NetworkManager m, int prefabId) { if (prefabId == 12) { var ii = new InputsIdentity(); all.Add(ii); return ii; } if (prefabId == 11) { var gi = new GenericIdentity(); all.Add(gi); return gi; } if (prefabId == 10) { var pk = new PackerIdentity(); all.Add(pk); return pk; } if (prefabId == 9) { var rc = new RacerIdentity(); all.Add(rc); return rc; } if (prefabId == 7) { var rg = new Rig(); all.Add(rg); return rg; } if (prefabId == 8) { var sp = new Speaker(); all.Add(sp); return sp; } if (prefabId == 6) { var rf = new Reflected(); all.Add(rf); return rf; } if (prefabId == 4) { var ph = new Phases(); all.Add(ph); return ph; } if (prefabId == 5) { var lb = new LodBox(); all.Add(lb); return lb; } if (prefabId == 3) { var tg = new Target(); all.Add(tg); return tg; } if (prefabId == 2) { var pw = new PawnIdentity(); all.Add(pw); return pw; } if (prefabId == 1) { var mv = new Mover(); all.Add(mv); return mv; } var t = new TestIdentity(); alive.Add(t); all.Add(t); return t; }
    public void Destroy(NetworkIdentity id) { all.Remove(id); if (id is TestIdentity t) alive.Remove(t); }
}

static class Program
{
    const int PH = 4, LB = 5, RF = 6, RG = 7, SP = 8, RC = 9, PK = 10, GI = 11, II = 12;
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
            await Pump(300, hostL, cN, cF);
            Check(((LodBox)bN).counter.v.value == box.counter.v.value && ((LodBox)bF).counter.v.value == before && box.IsObserver(cF.localPlayer.Value), $"near client follows ({((LodBox)bN).counter.v.value}), culled client frozen at {before} (now {((LodBox)bF).counter.v.value}) but still observes");
            avF.transform.position = new Vector3(8, 0, 0); await Pump(300, hostL, cN, cF);
            box.counter.v.value++; await Until(() => ((LodBox)bF).counter.v.value == box.counter.v.value, 3000, hostL, cN, cF);
            Check(box.lod.GetTier(cF.localPlayer.Value) == 0 && ((LodBox)bF).counter.v.value == box.counter.v.value, "far player moved close: tier 0 and deltas resume");
            hostL.Stop(); cN.Stop(); cF.Stop(); await Pump(300, hostL, cN, cF);
        }

        // NetworkReflection: fields/properties of a plain object replicate by name; tracked methods are callable as RPCs
        {
            var hostR = Make("hostR"); var cR = Make("cR"); var cR2 = Make("cR2"); hostR.roomName = cR.roomName = cR2.roomName = room + "-refl";
            hostR.StartHost(); await Until(() => hostR.isServer, 8000, hostR);
            var rf = (Reflected)hostR.Spawn(RF);
            cR.StartClient(); await Until(() => cR.TryGetIdentity(rf.objectId, out _), 8000, hostR, cR);
            cR.TryGetIdentity(rf.objectId, out var rfC1); var rfC = (Reflected)rfC1;
            Check(rfC.plain.hp == 5 && rfC.plain.title == "start", "full state of tracked members on spawn");
            rf.plain.hp = 42; rf.plain.title = "boss"; rf.plain.pos = new Vector3(1, 2, 3);
            await Until(() => rfC.plain.hp == 42 && rfC.plain.title == "boss", 3000, hostR, cR);
            Check(rfC.plain.hp == 42 && rfC.plain.title == "boss" && rfC.plain.pos.z == 3, "changed field, property and Vector3 replicated to the client");
            rfC.refl.Invoke("Heal", 8);
            await Until(() => rf.plain.healed == 1, 3000, hostR, cR);
            Check(rf.plain.hp == 50 && rf.plain.healed == 1, $"client called a Server-type tracked method on the server (hp {rf.plain.hp})");
            await Until(() => rfC.plain.hp == 50, 3000, hostR, cR);
            cR2.StartClient(); await Until(() => cR2.TryGetIdentity(rf.objectId, out _), 8000, hostR, cR, cR2); await Pump(200, hostR, cR, cR2);
            cR2.TryGetIdentity(rf.objectId, out var rfC2x); var rfC2 = (Reflected)rfC2x;
            Check(rfC2.plain.hp == 50 && rfC2.plain.title == "boss" && rfC.plain.hp == 50, "late joiner gets the current tracked state");
            rf.refl.Invoke("Whisper", cR2.localPlayer.Value, "psst");
            await Until(() => rfC2.plain.lastWhisper == "psst", 3000, hostR, cR, cR2);
            Check(rfC2.plain.lastWhisper == "psst" && rfC.plain.lastWhisper == null, "Target-type tracked method reached only the target");
            hostR.Stop(); cR.Stop(); cR2.Stop(); await Pump(300, hostR, cR, cR2);
        }

        // NetworkBones + NetworkAudioSource
        {
            var hostB = Make("hostB"); var cB = Make("cB"); hostB.roomName = cB.roomName = room + "-bones";
            hostB.StartHost(); await Until(() => hostB.isServer, 8000, hostB);
            var rig = (Rig)hostB.Spawn(RG); rig.b[0].localPosition = new Vector3(0, 1, 0); rig.b[2].localRotation = new Quaternion(0, 0.7071f, 0, 0.7071f);
            var spk = (Speaker)hostB.Spawn(SP);
            cB.StartClient(); await Until(() => cB.TryGetIdentity(rig.objectId, out _) && cB.TryGetIdentity(spk.objectId, out _), 8000, hostB, cB);
            cB.TryGetIdentity(rig.objectId, out var rigC1); var rigC = (Rig)rigC1; cB.TryGetIdentity(spk.objectId, out var spkC1); var spkC = (Speaker)spkC1;
            Check(rigC.bones.boneCount == 3 && Vector3.Distance(rigC.b[0].localPosition, new Vector3(0, 1, 0)) < 0.02f && Quaternion.Angle(rigC.b[2].localRotation, rig.b[2].localRotation) < 1f, $"bones full pose on spawn ({rigC.bones.boneCount} bones)");
            rig.b[1].localPosition = new Vector3(2, 3, 4);
            await Until(() => Vector3.Distance(rigC.b[1].localPosition, new Vector3(2, 3, 4)) < 0.02f, 3000, hostB, cB);
            Check(Vector3.Distance(rigC.b[1].localPosition, new Vector3(2, 3, 4)) < 0.02f && Vector3.Distance(rigC.b[0].localPosition, new Vector3(0, 1, 0)) < 0.02f, "moved bone delta replicated, untouched bone kept");
            spk.audio.volume.value = 0.3f; spk.audio.loop.value = true; spk.audio.Play(1);
            await Until(() => spkC.src.isPlaying && spkC.src.clip != null, 3000, hostB, cB);
            Check(spkC.src.isPlaying && spkC.src.clip.name == "b" && Math.Abs(spkC.src.volume - 0.3f) < 0.001f && spkC.src.loop, $"audio: Play(clip b) + settings replicated (vol {spkC.src.volume})");
            spk.audio.PlayOneShot(0, 0.5f);
            await Until(() => spkC.src.lastOneShot != null, 3000, hostB, cB);
            Check(spkC.src.lastOneShot.name == "a" && spkC.src.lastOneShotScale == 0.5f && spk.src.lastOneShot.name == "a", "one-shot ran on host and client");
            spk.audio.Stop(); await Until(() => !spkC.src.isPlaying, 3000, hostB, cB);
            Check(!spkC.src.isPlaying && !spkC.audio.isPlaying, "Stop replicated");
            hostB.Stop(); cB.Stop(); await Pump(300, hostB, cB);
        }

        // ValidatedSyncVar (server vetoes owner writes) + SyncBigData (chunked 40 KB payload, late joiner gets it whole)
        {
            var hostX = Make("hostX"); var cX = Make("cX"); var cX2 = Make("cX2"); hostX.roomName = cX.roomName = cX2.roomName = room + "-val";
            hostX.StartHost(); await Until(() => hostX.isServer, 8000, hostX);
            cX.StartClient(); await Until(() => hostX.players.Count == 2, 8000, hostX, cX);
            var rc = (RacerIdentity)hostX.Spawn(RC, cX.localPlayer);
            await Until(() => cX.TryGetIdentity(rc.objectId, out _), 5000, hostX, cX);
            cX.TryGetIdentity(rc.objectId, out var rcC1); var rcC = (RacerIdentity)rcC1;
            int rejected = 0; rcC.racer.speed.onRejected += _ => rejected++;
            rcC.racer.speed.value = 5; await Until(() => rc.racer.speed.value == 5, 3000, hostX, cX);
            Check(rc.racer.speed.value == 5 && rejected == 0, "owner proposal within limits accepted by the server");
            rcC.racer.speed.value = 50; await Until(() => rejected == 1, 3000, hostX, cX); await Pump(200, hostX, cX);
            Check(rc.racer.speed.value == 5 && rcC.racer.speed.value == 5 && rejected == 1, $"out-of-range proposal rejected and owner reverted (owner sees {rcC.racer.speed.value})");
            var payload = new byte[40_000]; new Random(7).NextBytes(payload);
            byte[] got = null; var progress = new List<float>(); rcC.racer.blob.onCompleted += d => got = d; rcC.racer.blob.onProgress += pr => progress.Add(pr);
            rc.racer.blob.Set(payload);
            await Until(() => got != null, 5000, hostX, cX);
            Check(got != null && got.Length == payload.Length && got.AsSpan().SequenceEqual(payload) && progress.Count >= 3 && progress[0] < 1f, $"40 KB streamed in chunks ({progress.Count} progress events), bytes identical");
            cX2.StartClient(); await Until(() => cX2.TryGetIdentity(rc.objectId, out _), 8000, hostX, cX, cX2); await Pump(300, hostX, cX, cX2);
            cX2.TryGetIdentity(rc.objectId, out var rcC2x); var rcC2 = (RacerIdentity)rcC2x;
            Check(rcC2.racer.blob.data.Length == payload.Length && rcC2.racer.blob.data.AsSpan().SequenceEqual(payload) && rcC2.racer.speed.value == 5, "late joiner receives the whole payload and the validated value");
            hostX.Stop(); cX.Stop(); cX2.Stop(); await Pump(300, hostX, cX, cX2);
        }

        // NetworkStatistics: client ping/jitter estimate, server per-player ping, throughput counters
        {
            var hostT = Make("hostT"); var cT = Make("cT"); hostT.roomName = cT.roomName = room + "-stats";
            hostT.StartHost(); await Until(() => hostT.isServer, 8000, hostT);
            cT.StartClient(); await Until(() => hostT.players.Count == 2, 8000, hostT, cT);
            await Until(() => cT.statistics.hasPingEstimate && hostT.statistics.PingOf(cT.localPlayer.Value) >= 0, 4000, hostT, cT);
            await Pump(1100, hostT, cT);
            Check(cT.statistics.hasPingEstimate && cT.statistics.ping >= 0 && cT.statistics.ping < 500, $"client ping estimate {cT.statistics.ping} ms (jitter {cT.statistics.jitter})");
            Check(hostT.statistics.PingOf(cT.localPlayer.Value) >= 0, $"server knows the client's ping ({hostT.statistics.PingOf(cT.localPlayer.Value)} ms)");
            Check(cT.statistics.upload > 0 && cT.statistics.download > 0 && cT.statistics.totalBytesIn > 0 && !cT.statistics.isConnectionStalled, $"throughput counters tick (up {cT.statistics.upload:0} B/s, down {cT.statistics.download:0} B/s)");
            hostT.Stop(); cT.Stop(); await Pump(300, hostT, cT);
        }

        // RPC packing: compression shrinks a 4 KB string, delta packing shrinks repeated state calls; payloads arrive intact
        {
            var hostK = Make("hostK"); var cK = Make("cK"); hostK.roomName = cK.roomName = room + "-pack";
            hostK.StartHost(); await Until(() => hostK.isServer, 8000, hostK);
            var pk = (PackerIdentity)hostK.Spawn(PK);
            cK.StartClient(); await Until(() => cK.TryGetIdentity(pk.objectId, out _), 8000, hostK, cK); await Pump(300, hostK, cK);
            cK.TryGetIdentity(pk.objectId, out var pkC1); var pkC = (PackerIdentity)pkC1;
            var big = new string('a', 4000) + "|" + string.Concat(Enumerable.Repeat("aurix", 200));
            long before = hostK.statistics.totalBytesOut; pk.packer.Big(big);
            await Until(() => pkC.packer.bigs.Count == 1, 3000, hostK, cK); long used = hostK.statistics.totalBytesOut - before;
            Check(pkC.packer.bigs[0] == big && used < 1000, $"compressed ObserversRpc: {big.Length} chars arrived intact using {used} bytes on the wire");
            var longName = new string('n', 600);
            pk.packer.State(new Vector3(1, 2, 3), 100, longName); await Until(() => pkC.packer.states.Count == 1, 3000, hostK, cK);
            before = hostK.statistics.totalBytesOut; pk.packer.State(new Vector3(1, 2, 3.5f), 99, longName);
            await Until(() => pkC.packer.states.Count == 2, 3000, hostK, cK); used = hostK.statistics.totalBytesOut - before;
            Check(pkC.packer.states[1].Item1.z == 3.5f && pkC.packer.states[1].Item2 == 99 && pkC.packer.states[1].Item3 == longName && used < 300, $"delta-packed ObserversRpc: second call (600-char name unchanged) used {used} bytes, values intact");
            hostK.Stop(); cK.Stop(); await Pump(300, hostK, cK);
        }

        // generic RPCs: type arguments travel with the call, woven prologue routes it, receiver closes the method
        {
            var hostG = Make("hostG"); var cG = Make("cG"); hostG.roomName = cG.roomName = room + "-gen";
            hostG.StartHost(); await Until(() => hostG.isServer, 8000, hostG);
            var gi = (GenericIdentity)hostG.Spawn(GI);
            cG.StartClient(); await Until(() => cG.TryGetIdentity(gi.objectId, out _), 8000, hostG, cG); await Pump(200, hostG, cG);
            cG.TryGetIdentity(gi.objectId, out var giC1); var giC = (GenericIdentity)giC1;
            gi.g.Show(42); gi.g.Show("hello"); gi.g.Show(new Vector3(1, 2, 3));
            await Until(() => giC.g.seen.Count == 3, 3000, hostG, cG);
            Check(gi.g.seen.Count == 3 && giC.g.seen.Count == 3 && giC.g.seen[0] is int i42 && i42 == 42 && giC.g.seen[1] as string == "hello" && giC.g.seen[2] is Vector3 v3 && v3.z == 3, $"generic ObserversRpc ran locally and on the client with int/string/Vector3 ({string.Join(",", giC.g.seen.Select(x => x.GetType().Name))})");
            giC.g.Push(3.5f); giC.g.Push("from client");
            await Until(() => gi.g.pushed.Count == 2, 3000, hostG, cG);
            Check(gi.g.pushed.Count == 2 && gi.g.pushed[0] == cG.localPlayer + ":3.5" && gi.g.pushed[1] == cG.localPlayer + ":from client" && giC.g.pushed.Count == 0, $"generic ServerRpc with RpcInfo reached the server ({string.Join(" | ", gi.g.pushed)})");
            hostG.Stop(); cG.Stop(); await Pump(300, hostG, cG);
        }

        // async factory (Addressables-style): spawn + messages for a not-yet-loaded prefab stay pending, then apply in order
        {
            var hostA = Make("hostA"); var cA = Make("cA"); hostA.roomName = cA.roomName = room + "-async";
            var delayed = new DelayedFactory { inner = cA.factory, delayPumps = 25 }; cA.factory = delayed;
            hostA.StartHost(); await Until(() => hostA.isServer, 8000, hostA);
            cA.StartClient(); await Until(() => hostA.players.Count == 2, 8000, hostA, cA);
            var obj = (TestIdentity)hostA.Spawn(0); obj.player.health.value = 77;
            for (int i = 0; i < 12; i++) { hostA.NetworkUpdate(0.01f); cA.NetworkUpdate(0.01f); await Task.Delay(15); }
            obj.player.health.value = 55; hostA.SetOwnership(obj, cA.localPlayer);
            for (int i = 0; i < 12; i++) { hostA.NetworkUpdate(0.01f); cA.NetworkUpdate(0.01f); await Task.Delay(15); }
            Check(cA.pendingSpawnCount == 1 && !cA.TryGetIdentity(obj.objectId, out _), "spawn is pending while the prefab loads; later messages are queued");
            for (int i = 0; i < 40 && !cA.TryGetIdentity(obj.objectId, out _); i++) { hostA.NetworkUpdate(0.01f); cA.NetworkUpdate(0.01f); delayed.Pump(cA); await Task.Delay(15); }
            cA.TryGetIdentity(obj.objectId, out var objC1); var objC = objC1 as TestIdentity;
            Check(objC != null && cA.pendingSpawnCount == 0 && objC.player.health.value == 55 && objC.isOwner, $"after load: spawned, queued Sync and Owner applied (health {objC?.player.health.value}, owner {objC?.isOwner})");
            hostA.Stop(); cA.Stop(); await Pump(300, hostA, cA);
        }

        // SyncInput (owner → server only) + SyncTexture / SyncFile over SyncBigData
        {
            var hostI = Make("hostI"); var cI = Make("cI"); var cI2 = Make("cI2"); hostI.roomName = cI.roomName = cI2.roomName = room + "-input";
            hostI.StartHost(); await Until(() => hostI.isServer, 8000, hostI);
            cI.StartClient(); cI2.StartClient(); await Until(() => hostI.players.Count == 3, 8000, hostI, cI, cI2);
            var ii = (InputsIdentity)hostI.Spawn(II, cI.localPlayer);
            await Until(() => cI.TryGetIdentity(ii.objectId, out _) && cI2.TryGetIdentity(ii.objectId, out _), 5000, hostI, cI, cI2);
            cI.TryGetIdentity(ii.objectId, out var iiC1); var iiC = (InputsIdentity)iiC1; cI2.TryGetIdentity(ii.objectId, out var iiC2x); var iiC2 = (InputsIdentity)iiC2x;
            int serverSaw = 0; ii.inputs.ability.onChanged += v => serverSaw = v;
            iiC.inputs.ability.value = 3;
            await Until(() => serverSaw == 3, 3000, hostI, cI, cI2); await Pump(200, hostI, cI, cI2);
            Check(serverSaw == 3 && ii.inputs.ability.value == 3 && iiC2.inputs.ability.value == 0, $"SyncInput reached the server (3) and not the other client ({iiC2.inputs.ability.value})");
            var png = new byte[12_000]; new Random(3).NextBytes(png); var tex = new Texture2D(64, 64) { raw = png };
            Texture2D gotTex = null; iiC2.inputs.tex.onChanged += t => gotTex = t;
            string gotName = null; byte[] gotFile = null; iiC2.inputs.file.onChanged += (n, b) => { gotName = n; gotFile = b; };
            ii.inputs.tex.Set(tex); ii.inputs.file.Set("level.json", System.Text.Encoding.UTF8.GetBytes("{\"rooms\":3}"));
            await Until(() => gotTex != null && gotFile != null, 5000, hostI, cI, cI2);
            Check(gotTex != null && gotTex.raw.AsSpan().SequenceEqual(png) && gotName == "level.json" && System.Text.Encoding.UTF8.GetString(gotFile) == "{\"rooms\":3}", $"SyncTexture (12 KB) and SyncFile arrived intact ({gotName})");
            hostI.Stop(); cI.Stop(); cI2.Stop(); await Pump(300, hostI, cI, cI2);
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
