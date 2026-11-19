using System.Collections.Generic;
using System;
using System.IO;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Net.Sockets;
using System.Net.WebSockets;
using System.Reflection;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Aurix.Net;
using Aurix.Net.Managed;
using Aurix.Net.Unity;
using UnityEngine;

// A TCP relay to the node that a check can cut: disposing it closes every connection made through it.
sealed class Relay : IDisposable
{
    readonly TcpListener _listener = new TcpListener(IPAddress.Loopback, 0);
    readonly List<TcpClient> _ends = new List<TcpClient>();
    public string Url => "http://127.0.0.1:" + ((IPEndPoint)_listener.LocalEndpoint).Port;
    public Relay() { _listener.Start(); _ = Accept(); }
    async Task Accept()
    {
        try
        {
            while (true)
            {
                var near = await _listener.AcceptTcpClientAsync(); var far = new TcpClient();
                await far.ConnectAsync(IPAddress.Loopback, 8090);
                lock (_ends) { _ends.Add(near); _ends.Add(far); }
                _ = near.GetStream().CopyToAsync(far.GetStream()); _ = far.GetStream().CopyToAsync(near.GetStream());
            }
        }
        catch (Exception error) when (error is SocketException || error is ObjectDisposedException) { }
    }
    public void Dispose() { _listener.Stop(); lock (_ends) foreach (var end in _ends) end.Close(); }
}

/// A WebSocket relay to the node that can hold back the binary frames of its client: the node's
/// reliable frames reach the client, the client's acknowledgements do not reach the node.
sealed class GagRelay : IDisposable
{
    readonly HttpListener _listener = new HttpListener();
    readonly CancellationTokenSource _stop = new CancellationTokenSource();
    public string Url { get; }
    public volatile bool Gagged;
    // the client's socket is not read: what it sends fills the buffers and its sends stop completing
    public volatile bool Stalled;
    public volatile int DropOver = int.MaxValue;

    public GagRelay()
    {
        var probe = new TcpListener(IPAddress.Loopback, 0); probe.Start();
        int port = ((IPEndPoint)probe.LocalEndpoint).Port; probe.Stop();
        Url = "http://127.0.0.1:" + port;
        _listener.Prefixes.Add(Url + "/"); _listener.Start(); _ = Accept();
    }

    async Task Accept()
    {
        try
        {
            while (true)
            {
                var context = await _listener.GetContextAsync();
                if (!context.Request.IsWebSocketRequest) { context.Response.StatusCode = 404; context.Response.Close(); continue; }
                var near = (await context.AcceptWebSocketAsync(null)).WebSocket; var far = new ClientWebSocket();
                await far.ConnectAsync(new Uri("ws://127.0.0.1:8090" + context.Request.RawUrl), _stop.Token);
                _ = Pump(near, far, true); _ = Pump(far, near, false);
            }
        }
        catch (Exception error) when (error is HttpListenerException || error is ObjectDisposedException || error is OperationCanceledException) { }
    }

    async Task Pump(WebSocket from, WebSocket to, bool fromClient)
    {
        var buffer = new byte[1 << 16]; var message = new MemoryStream();
        try
        {
            while (true)
            {
                while (fromClient && Stalled) await Task.Delay(20, _stop.Token);
                var part = await from.ReceiveAsync(new ArraySegment<byte>(buffer), _stop.Token);
                if (part.MessageType == WebSocketMessageType.Close) break;
                message.Write(buffer, 0, part.Count);
                if (!part.EndOfMessage) continue;
                bool held = fromClient && part.MessageType == WebSocketMessageType.Binary && (Gagged || message.Length > DropOver);
                if (!held) await to.SendAsync(new ArraySegment<byte>(message.GetBuffer(), 0, (int)message.Length), part.MessageType, true, _stop.Token);
                message.SetLength(0);
            }
        }
        catch (Exception error) when (error is WebSocketException || error is ObjectDisposedException || error is OperationCanceledException) { }
        to.Abort();
    }

    public void Dispose() { _stop.Cancel(); _listener.Close(); }
}

// Stands in for the engine's main thread: what a client awaits there resumes only when the game runs.
sealed class GameThread : SynchronizationContext
{
    readonly System.Collections.Concurrent.ConcurrentQueue<(SendOrPostCallback, object)> _posted = new System.Collections.Concurrent.ConcurrentQueue<(SendOrPostCallback, object)>();

    public override void Post(SendOrPostCallback callback, object state) => _posted.Enqueue((callback, state));

    public void Pump() { while (_posted.TryDequeue(out var posted)) posted.Item1(posted.Item2); }

    public T Run<T>(Func<Task<T>> start)
    {
        var task = start();
        while (!task.IsCompleted) { Pump(); Thread.Sleep(1); }
        return task.GetAwaiter().GetResult();
    }
}

// A game whose state is a number: the commands of a tick in their order, then the players' inputs.
sealed class Toy : ISimulation<ulong>
{
    public ulong State;
    public void Step(uint tick, Dictionary<ushort, byte[]> inputs, IReadOnlyList<LockstepCommand> commands)
    {
        foreach (var c in commands) unchecked { State = State * 131 + c.Actor * 1000UL + c.Data[0]; }
        foreach (var kv in inputs.OrderBy(k => k.Key)) unchecked { State = State * 31 + tick * 1000UL + kv.Key * 10UL + (kv.Value.Length > 0 ? kv.Value[0] : 0UL); }
        unchecked { State = State * 7 + tick; }
    }
    public ulong Snapshot() => State;
    public void Restore(ulong snapshot) { State = snapshot; }
}

class P
{
    // NET_LIVE_REQUIRED: a part that cannot run fails the check instead of being skipped
    static readonly bool Required = Environment.GetEnvironmentVariable("NET_LIVE_REQUIRED") != null;

    static bool Skip(string what) { Console.WriteLine(what); return !Required; }

    static async Task<string> Grant(HttpClient h, string key, string pid)
    {
        var req = new HttpRequestMessage(HttpMethod.Post, "http://127.0.0.1:8090/v1/grants");
        req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        req.Content = new StringContent("{\"player_id\":\"" + pid + "\",\"name\":\"" + pid + "\"}", Encoding.UTF8, "application/json");
        var r = await h.SendAsync(req); var body = await r.Content.ReadAsStringAsync();
        if (!r.IsSuccessStatusCode) throw new Exception(body);
        int i = body.IndexOf("\"grant\":\"") + 9; return body.Substring(i, body.IndexOf('"', i) - i);
    }
    static async Task<int> Main()
    {
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        var h = new HttpClient();
        var room = "cs-" + DateTime.UtcNow.Ticks;
        var a = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-a"));
        var b = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-b"));
        if (a.PlayerId != "cs-a" || a.PlayerName != "cs-a" || b.PlayerId != "cs-b") throw new Exception("player: id " + a.PlayerId + ", name " + a.PlayerName);
        int rel = 0, unrel = 0, big = 0, bigLen = 0; string ctl = "";
        b.OnEvent += f => { if (f.Code == 1) rel++; else if (f.Code == 2) unrel++; else if (f.Code == 3) { big++; bigLen = f.Payload.Count; } };
        b.OnControl += j => { if (j.Contains("player_joined")) ctl += "J"; };
        int aa = await a.JoinOrCreateAsync(room, "relay", 8);
        bool wayBack = a.LastJoinJson == "{\"t\":\"join\",\"name\":\"" + room + "\"}";
        int ab = await b.JoinAsync(room);
        Console.WriteLine($"joined actors a={aa} b={ab} mode={a.RoomMode}");
        a.SendEvent(1, Encoding.UTF8.GetBytes("hello"), Target.Others, reliable: true);
        for (int i = 0; i < 10; i++) a.SendEvent(2, new byte[] { (byte)i }, Target.Others, reliable: false);
        a.SendEvent(3, new byte[5000], Target.Others, reliable: true);
        a.Flush();
        var until = DateTime.UtcNow.AddSeconds(2);
        while (DateTime.UtcNow < until) { a.Poll(); b.Poll(); await Task.Delay(5); }
        Console.WriteLine($"relay: reliable={rel}/1 unreliable={unrel}/10 fragmented={big}/1 ({bigLen} bytes) control={ctl}");
        await a.LeaveAsync(); await b.LeaveAsync(); a.Dispose(); b.Dispose();

        var s = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-s"));
        int deltas = 0; s.OnData += (ch, tick, p) => { if ((p[0] & 0xf0) == 0x10) deltas++; };
        await s.JoinOrCreateAsync("cs-sync-" + room, "sync", 8, "{\"tick_hz\":60}");
        until = DateTime.UtcNow.AddSeconds(2); int sent = 0;
        while (DateTime.UtcNow < until) { s.SendData(Channel.Unreliable, new byte[] { 0x10, 0, 0, 0, 0, 1, 3, 127, 0, 0 }); sent++; s.Flush(); s.Poll(); await Task.Delay(16); }
        Console.WriteLine($"sync: inputs sent={sent} deltas received={deltas} (expect ~120 in 2 s)");
        await s.LeaveAsync(); s.Dispose();
        // owner-authoritative sync room with a custom schema: A moves its avatar, B decodes deltas
        var idl = "component Transform {\\n  pos: vec3 @range(-100, 100, 0.01)\\n  rot: quat\\n}\\ncomponent Tag {\\n  name: string(16)\\n  hp: u8\\n}\\n";
        var schema = Schema.Parse(idl.Replace("\\n", "\n"));
        var wa = new SyncWorld(schema); var wb = new SyncWorld(schema);
        var oa = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-oa"));
        var ob = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-ob"));
        oa.OnData += (ch, t, p) => { var ack = wa.Receive(p); if (ack != null) oa.SendData(Channel.Unreliable, ack); };
        ob.OnData += (ch, t, p) => { var ack = wb.Receive(p); if (ack != null) ob.SendData(Channel.Unreliable, ack); };
        int spawns = 0, changes = 0; wb.OnSpawn += e => spawns++; wb.OnChange += e => changes++;
        var pending = new PendingSpawns(); SpawnAnswer? made = null;
        oa.OnData += (ch, t, p) => { if (SyncWorld.ReadSpawnAnswer(p) is SpawnAnswer answer && pending.Take(answer) && answer.Tag != 0) made = answer; };
        int actA = await oa.JoinOrCreateAsync("cs-owner-" + room, "sync", 8, "{\"schema\":\"" + idl + "\",\"tick_hz\":60}");
        var hash = Aurix.Net.Managed.ManagedClientTestHooks.Str(oa.JoinedJson, "schema_hash");
        await ob.JoinAsync("cs-owner-" + room, hash);
        var tr = schema.ByName("Transform"); var tag = schema.ByName("Tag");
        until = DateTime.UtcNow.AddSeconds(2); float x = 0;
        while (DateTime.UtcNow < until)
        {
            x = Math.Min(5f, x + 0.1f);
            var pos = ComponentValue.Zero(tr); pos.Fields[0] = new Value { X = x, Y = 1.5f, Z = -2 }; pos.Fields[1] = new Value { X = 0, Y = 0.7071f, Z = 0, W = 0.7071f };
            var tg = ComponentValue.Zero(tag); tg.Fields[0] = new Value { S = "alice" }; tg.Fields[1] = new Value { U = 77 };
            var body = wa.EncodeWrites(new List<(ComponentDef, ComponentValue)> { (tr, pos), (tag, tg) });
            oa.SendData(Channel.Unreliable, SyncWorld.EncodeInput(wa.ServerTick + 2, new[] { body }));
            oa.Flush(); ob.Flush(); oa.Poll(); ob.Poll(); await Task.Delay(16);
        }
        NetEntity seen = null; foreach (var e in wb.Entities.Values) if (e.Owner == actA) seen = e;
        var sp = seen?.Get(tr)?.Fields[0]; var sq = seen?.Get(tr)?.Fields[1]; var st = seen?.Get(tag);
        Console.WriteLine($"schema sync: entities on B={wb.Entities.Count} spawns={spawns} changes={changes} gaps={wb.Gaps} A.pos=({sp?.X:F2},{sp?.Y:F2},{sp?.Z:F2}) rot.y={sq?.Y:F3} tag={st?.Fields[0].S}/{st?.Fields[1].U}");
        bool schemaOk = seen != null && wb.Entities.Count == 2 && Math.Abs((sp?.X ?? 0) - 5f) < 0.02 && Math.Abs((sp?.Z ?? 0) + 2f) < 0.02 && Math.Abs((sq?.Y ?? 0) - 0.7071f) < 0.01 && st?.Fields[0].S == "alice" && st?.Fields[1].U == 77;
        // A spawns a second entity under a tag: the room answers with the entity it made and has named A's avatar
        uint SendSpawn(byte[] ops) { oa.SendData(Channel.Unreliable, SyncWorld.EncodeInput(wa.ServerTick + 2, new[] { ops })); return wa.ServerTick + 2; }
        uint spawnTag = pending.NextTag(oa.NextSpawnTag ?? 0);
        var spawnOps = wa.EncodeOps(new[] { SyncOp.SpawnTaggedEntity(spawnTag) });
        pending.Sent(spawnTag, spawnOps, SendSpawn(spawnOps));
        until = DateTime.UtcNow.AddSeconds(3);
        while ((made == null || !wa.Entities.ContainsKey(made.Value.Entity)) && DateTime.UtcNow < until)
        {
            pending.Repeat(wa.ServerTick, 30, SendSpawn);
            oa.Flush(); ob.Flush(); oa.Poll(); ob.Poll(); await Task.Delay(16);
        }
        bool spawnOk = oa.NextSpawnTag != null && made?.Tag == spawnTag && made?.Status == SpawnStatus.Created && oa.Avatar != null && made?.Entity != oa.Avatar
            && wa.Entities.TryGetValue(made.Value.Entity, out var spawned) && spawned.Owner == actA && wa.Entities.ContainsKey(oa.Avatar.Value);
        Console.WriteLine($"tagged spawn: tag={spawnTag} status={made?.Status} entity={made?.Entity} avatar={oa.Avatar} -> {(spawnOk ? "ok" : "FAIL")}");
        // over a link that loses data frames (NET_LOSSY_HTTP, see README): a write with nothing after it still arrives
        bool tailOk = true; var lossy = Environment.GetEnvironmentVariable("NET_LOSSY_HTTP");
        if (lossy != null)
        {
            var wc = new SyncWorld(schema); var oc = await ManagedClient.ConnectAsync(lossy, await Grant(h, key, "cs-oc"));
            oc.OnData += (ch, t, p) => { var ack = wc.Receive(p); if (ack != null) oc.SendData(Channel.Unreliable, ack); };
            int actC = await oc.JoinAsync("cs-owner-" + room, hash);
            var frames = new InputFrames(); int arrived = 0; const int rounds = 30;
            for (int round = 0; round <= rounds; round++)
            {
                if (round > 0)
                {
                    var pos = ComponentValue.Zero(tr); pos.Fields[0] = new Value { X = round, Y = 1.5f, Z = -2 }; pos.Fields[1] = new Value { W = 1 };
                    var body = wc.EncodeWrites(new List<(ComponentDef, ComponentValue)> { (tr, pos) });
                    oc.SendData(Channel.Unreliable, frames.Frame(Math.Max(wc.ServerTick + 2, frames.LastTick + 1), body, wc.ServerTick));
                }
                for (int i = 0; i < 15; i++)
                {
                    oc.Flush(); ob.Flush(); await Task.Delay(16); oc.Poll(); ob.Poll();
                    var tail = frames.Tail(wc.ServerTick);
                    if (tail != null) oc.SendData(Channel.Unreliable, tail);
                }
                foreach (var e in wb.Entities.Values) if (round > 0 && e.Owner == actC && Math.Round(e.Get(tr)?.Fields[0].X ?? 0) == round) arrived++;
            }
            tailOk = arrived == rounds;
            Console.WriteLine($"lossy link: {arrived} of {rounds} single writes arrived -> {(tailOk ? "ok" : "FAIL")}");
            await oc.LeaveAsync(); oc.Dispose();
        }
        else tailOk = Skip("lossy link: skipped (NET_LOSSY_HTTP not set)");
        await oa.LeaveAsync(); await ob.LeaveAsync(); oa.Dispose(); ob.Dispose();
        // lockstep: A (master) and B confirm identical input streams, C joins late and catches up contiguously
        var lsRoom = "cs-ls-" + room; var clients = new List<ManagedClient>(); var ls = new List<LockstepClient>(); var journals = new List<List<string>>(); var seenTicks = new List<HashSet<uint>>();
        int ckReqs = 0, restores = 0; uint cStart = 0;
        for (int i = 0; i < 3; i++)
        {
            var c = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-ls-" + i));
            var cl = c; var l = new LockstepClient((ch, b) => cl.SendData(ch, b)); var j = new List<string>(); var sn = new HashSet<uint>(); int idx = i;
            l.OnTick += (t, inputs) => { if (!sn.Add(t)) j.Add("DUP" + t); var sb = new StringBuilder(); sb.Append(t).Append(':'); foreach (var kv in inputs) sb.Append(kv.Key).Append('=').Append(kv.Value.Length > 0 ? kv.Value[0] : -1).Append(','); j.Add(sb.ToString()); if (t % 10 == 0) l.ReportHash(t, 1234); };
            l.OnRestore += (t, hh, st) => { restores++; if (idx == 2) cStart = t; };
            l.OnCheckpointRequested += t => { ckReqs++; l.SendCheckpoint(Math.Min(t, l.ConfirmedTick), 0xA0, new byte[64]); };
            c.OnData += (ch, t, p) => l.Receive(p);
            clients.Add(c); ls.Add(l); journals.Add(j); seenTicks.Add(sn);
        }
        await clients[0].JoinOrCreateAsync(lsRoom, "lockstep", 8, "{\"tick_hz\":60}");
        await clients[1].JoinAsync(lsRoom);
        if (clients[0].Seed == null || clients[0].Seed != clients[1].Seed) throw new Exception($"lockstep seed: {clients[0].Seed} and {clients[1].Seed}");
        until = DateTime.UtcNow.AddSeconds(8.5); bool cJoined = false; int frame = 0;
        while (DateTime.UtcNow < until)
        {
            frame++;
            if (!cJoined && DateTime.UtcNow > until.AddSeconds(-2)) { cJoined = true; await clients[2].JoinAsync(lsRoom); }
            for (int i = 0; i < (cJoined ? 3 : 2); i++) { ls[i].SendInput(new[] { (byte)(i * 16 + frame % 7) }); clients[i].Flush(); clients[i].Poll(); }
            await Task.Delay(16);
        }
        for (int i = 0; i < 3; i++) clients[i].Poll();
        // compare A vs B on the common range, and C vs A from C's catch-up start
        int cmpAB = 0, cmpAC = 0; bool sameAB = true, sameAC = true;
        var mapA = new Dictionary<uint, string>(); foreach (var e in journals[0]) { var k = uint.Parse(e.Substring(0, e.IndexOf(':'))); mapA[k] = e; }
        foreach (var e in journals[1]) { var k = uint.Parse(e.Substring(0, e.IndexOf(':'))); if (mapA.TryGetValue(k, out var ea)) { cmpAB++; if (ea != e) sameAB = false; } }
        foreach (var e in journals[2]) { var k = uint.Parse(e.Substring(0, e.IndexOf(':'))); if (mapA.TryGetValue(k, out var ea)) { cmpAC++; if (ea != e) sameAC = false; } }
        bool contiguousC = true; uint exp = cStart; foreach (var e in journals[2]) { var k = uint.Parse(e.Substring(0, e.IndexOf(':'))); if (k != exp) { contiguousC = false; break; } exp++; }
        Console.WriteLine($"lockstep: A ticks={journals[0].Count} B ticks={journals[1].Count} C ticks={journals[2].Count} from {cStart} (restores={restores}, checkpoint reqs answered={ckReqs}) A==B on {cmpAB}: {sameAB}, A==C on {cmpAC}: {sameAC}, C contiguous: {contiguousC}, dropped={ls[0].Dropped + ls[1].Dropped + ls[2].Dropped}");
        bool lsOk = journals[0].Count > 150 && sameAB && sameAC && contiguousC && cmpAC > 60 && restores >= 1;
        foreach (var c in clients) { await c.LeaveAsync(); c.Dispose(); }
        // leave a room and join another one on the same client
        var ra = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-ra"));
        var rb = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-rb"));
        await ra.JoinOrCreateAsync(room + "-first", "relay", 8);
        for (int i = 0; i < 5; i++) ra.SendEvent(1, new byte[] { (byte)i }, Target.All, reliable: true);
        until = DateTime.UtcNow.AddMilliseconds(300); while (DateTime.UtcNow < until) { ra.Poll(); await Task.Delay(5); }
        await ra.LeaveAsync();
        if (ra.Actor != -1 || ra.JoinedJson != null || ra.RoomMode != null || ra.Seed != null) throw new Exception("leave: the client still describes the room it left");
        until = DateTime.UtcNow.AddMilliseconds(300); while (DateTime.UtcNow < until) { ra.Poll(); await Task.Delay(5); }
        int again = 0; rb.OnEvent += f => { if (f.Code == 2) again++; };
        await ra.JoinOrCreateAsync(room + "-second", "relay", 8); await rb.JoinAsync(room + "-second");
        for (int i = 0; i < 5; i++) ra.SendEvent(2, new byte[] { (byte)i }, Target.Others, reliable: true);
        until = DateTime.UtcNow.AddSeconds(3); while (DateTime.UtcNow < until && again < 5) { ra.Poll(); rb.Poll(); await Task.Delay(5); }
        Console.WriteLine($"second room on the same client: reliable={again}/5");
        // events on the unsequenced channel arrive on it
        int unsequenced = 0; rb.OnData += (channel, _, payload) => { if (channel == Channel.UnreliableUnsequenced) unsequenced++; };
        for (int i = 0; i < 10; i++) { ra.SendEventOn(Channel.UnreliableUnsequenced, 9, new[] { (byte)i }); ra.Flush(); ra.Poll(); rb.Poll(); await Task.Delay(16); }
        until = DateTime.UtcNow.AddSeconds(2); while (DateTime.UtcNow < until && unsequenced < 10) { ra.Poll(); rb.Poll(); await Task.Delay(5); }
        Console.WriteLine($"unsequenced channel: events={unsequenced}/10");
        // two calls to the backend in flight: each gets its own answer
        var first = ra.RpcAsync("first"); var second = ra.RpcAsync("second");
        if (ManagedClientTestHooks.Num(await first, "id") == ManagedClientTestHooks.Num(await second, "id")) throw new Exception("rpc: two calls in flight got the same answer");
        // a tick's commands come with their senders, before the tick and apart from the players' inputs
        var apart = new List<(ManagedClient client, LockstepClient ls, List<string> heard)>(); int late = 0, asPlayer = 0;
        foreach (var name in new[] { "cs-ca", "cs-cb" })
        {
            var client = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, name));
            await client.JoinOrCreateAsync(room + "-apart", "lockstep", 8);
            var lockstep = new LockstepClient((ch, b) => client.SendData(ch, b)); var heard = new List<string>(); long stepped = -1;
            lockstep.OnCommand += (t, actor, data) => { heard.Add($"{t}:{actor}:{data[0]}"); if (t <= stepped) late++; };
            lockstep.OnTick += (t, inputs) => { stepped = t; if (inputs.ContainsKey(65535)) asPlayer++; };
            client.OnData += (ch, t, p) => lockstep.Receive(p);
            apart.Add((client, lockstep, heard));
        }
        async Task ApartFrames(Func<bool> done)
        {
            var end = DateTime.UtcNow.AddSeconds(3);
            while (DateTime.UtcNow < end && !done()) { foreach (var peer in apart) { peer.ls.SendInput(new byte[] { 1 }); peer.client.Poll(); } await Task.Delay(16); }
        }
        int ticksSeen = 0; apart[0].ls.OnTick += (t, inputs) => ticksSeen++;
        await ApartFrames(() => ticksSeen >= 10);
        apart[0].ls.SendCommand(new byte[] { 1 }); apart[0].ls.SendCommand(new byte[] { 2 }); apart[1].ls.SendCommand(new byte[] { 3 });
        await ApartFrames(() => apart[0].heard.Count >= 3 && apart[1].heard.Count >= 3);
        string fromFirst = $":{apart[0].client.Actor}:", fromSecond = $":{apart[1].client.Actor}:";
        var own = apart[0].heard.Where(c => c.Contains(fromFirst)).Select(c => c.Substring(c.LastIndexOf(':') + 1)).ToArray();
        if (apart[0].heard.Count != 3 || !apart[0].heard.SequenceEqual(apart[1].heard) || string.Join(",", own) != "1,2" || apart[0].heard.Count(c => c.Contains(fromSecond) && c.EndsWith(":3")) != 1 || late != 0 || asPlayer != 0)
            throw new Exception($"lockstep commands apart from inputs: a=[{string.Join(" ", apart[0].heard)}] b=[{string.Join(" ", apart[1].heard)}] late={late} among the inputs={asPlayer}");
        Console.WriteLine($"lockstep commands: {apart[0].heard.Count} with their senders, the same on both players, before their tick, none among the players' inputs");
        foreach (var peer in apart) peer.client.Dispose();
        // a client that runs ahead of the confirmed ticks and one that steps in OnTick agree on a command
        var aheadClient = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-ahead"));
        await aheadClient.JoinOrCreateAsync(room + "-ahead", "lockstep", 2, "{\"tick_hz\":30}");
        var plainClient = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-plain"));
        await plainClient.JoinOrCreateAsync(room + "-ahead", "lockstep", 2, "{\"tick_hz\":30}");
        var aheadLs = new LockstepClient((ch, b) => aheadClient.SendData(ch, b)); var plainLs = new LockstepClient((ch, b) => plainClient.SendData(ch, b));
        var aheadSim = new Toy(); var plainSim = new Toy(); var rollback = new LockstepRollback<ulong>(aheadSim, (ushort)aheadClient.Actor, 0, 8);
        var aheadHashes = new Dictionary<uint, ulong>(); var plainHashes = new Dictionary<uint, ulong>(); var aheadHeard = new List<string>(); var plainHeard = new List<string>();
        var plainCommands = new List<LockstepCommand>(); int dumpsAsked = 0;
        aheadLs.OnTick += (t, inputs) =>
        {
            foreach (var c in aheadLs.Commands) aheadHeard.Add($"{t}:{c.Actor}:{c.Data[0]}");
            rollback.Confirm(t, inputs, aheadLs.Commands);
            aheadHashes[t] = rollback.ConfirmedSnapshot; aheadLs.ReportHash(t, rollback.ConfirmedSnapshot);
        };
        plainLs.OnCommand += (t, actor, data) => { plainHeard.Add($"{t}:{actor}:{data[0]}"); plainCommands.Add(new LockstepCommand(actor, data)); };
        plainLs.OnTick += (t, inputs) => { plainSim.Step(t, inputs, plainCommands); plainCommands.Clear(); plainHashes[t] = plainSim.State; plainLs.ReportHash(t, plainSim.State); };
        aheadLs.OnDumpRequested += t => dumpsAsked++; plainLs.OnDumpRequested += t => dumpsAsked++;
        aheadClient.OnData += (ch, t, p) => aheadLs.Receive(p); plainClient.OnData += (ch, t, p) => plainLs.Receive(p);
        for (int step = 1; step <= 120; step++)
        {
            var input = new byte[] { 3 }; long stamped = aheadLs.SendInput(input);
            if (stamped >= 0) rollback.LocalInput((uint)stamped, input);
            rollback.Predict(aheadLs.ServerTick + aheadLs.InputDelay);
            plainLs.SendInput(new byte[] { 4 });
            if (step == 30) aheadLs.SendCommand(new byte[] { 100 });
            if (step == 60) plainLs.SendCommand(new byte[] { 50 });
            aheadClient.Poll(); plainClient.Poll(); await Task.Delay(33);
        }
        for (int i = 0; i < 10; i++) { aheadClient.Poll(); plainClient.Poll(); await Task.Delay(30); }
        if (aheadHeard.Count != 2 || !aheadHeard.SequenceEqual(plainHeard) || rollback.PredictedTicks == 0 || rollback.Rollbacks < 2)
            throw new Exception($"rollback command: ahead=[{string.Join(" ", aheadHeard)}] plain=[{string.Join(" ", plainHeard)}] predicted={rollback.PredictedTicks} rollbacks={rollback.Rollbacks}");
        uint lastCommand = uint.Parse(aheadHeard[1].Split(':')[0]); int sameTicks = 0, afterCommands = 0;
        foreach (var kv in aheadHashes)
        {
            if (!plainHashes.TryGetValue(kv.Key, out var other)) continue;
            sameTicks++; if (kv.Key >= lastCommand) afterCommands++;
            if (kv.Value != other) throw new Exception($"rollback command: the confirmed state differs at tick {kv.Key}: {kv.Value} and {other}");
        }
        if (sameTicks <= 60 || afterCommands <= 10 || dumpsAsked != 0) throw new Exception($"rollback command: compared={sameTicks} after the commands={afterCommands} dump requests={dumpsAsked}");
        Console.WriteLine($"rollback command: commands at ticks {string.Join(",", aheadHeard.Select(c => c.Split(':')[0]))}, {sameTicks} ticks equal ({afterCommands} after), {rollback.Rollbacks} rollbacks, no dump asked");
        aheadClient.Dispose(); plainClient.Dispose();
        // a command the node refuses comes back as a control error
        var cmd = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-cmd"));
        await cmd.JoinOrCreateAsync(room + "-cmd", "lockstep", 8);
        var cmdLs = new LockstepClient((ch, b) => cmd.SendData(ch, b)); var refusals = new List<string>(); int carried = 0;
        cmdLs.OnCommand += (t, actor, data) => carried++;
        cmd.OnData += (ch, t, p) => cmdLs.Receive(p);
        cmd.OnControl += json => { if (ManagedClientTestHooks.Str(json, "t") == "error") refusals.Add(ManagedClientTestHooks.Str(json, "code") + ": " + ManagedClientTestHooks.Str(json, "message")); };
        // past the sender's own check, the way an older client would send it
        var tooLong = new byte[2 + LockstepClient.MaxCommand]; tooLong[0] = 0x28; cmd.SendData(Channel.Reliable, tooLong);
        until = DateTime.UtcNow.AddSeconds(2); while (DateTime.UtcNow < until && refusals.Count == 0) { cmdLs.SendInput(new byte[] { 1 }); cmd.Poll(); await Task.Delay(16); }
        if (refusals.Count != 1 || refusals[0] != "invalid_request: invalid request: a lockstep command takes 1 to 251 bytes, got 252") throw new Exception("command over the limit: " + string.Join(" | ", refusals));
        refusals.Clear();
        // 40 commands of 200 bytes in one go: a tick carries one, 4096 bytes may wait, the rest is refused
        for (int i = 0; i < 40; i++) cmdLs.SendCommand(Enumerable.Repeat((byte)i, 200).ToArray());
        until = DateTime.UtcNow.AddSeconds(2); while (DateTime.UtcNow < until) { cmdLs.SendInput(new byte[] { 1 }); cmd.Poll(); await Task.Delay(16); }
        if (refusals.Count == 0 || refusals.Any(r => r != "rate_limited: rate limited") || carried + refusals.Count != 40) throw new Exception($"command flood: carried={carried} refused={refusals.Count} {refusals.FirstOrDefault()}");
        Console.WriteLine($"lockstep commands the node refuses: over the limit invalid_request, flood carried={carried} rate_limited={refusals.Count}");
        cmd.Dispose();
        // the node takes a client out of its room: the client no longer describes it
        var gone = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-gone"));
        await gone.JoinOrCreateAsync(room + "-gone", "lockstep", 8);
        string why = null; gone.OnControl += json => { if (ManagedClientTestHooks.Str(json, "t") == "left") why = ManagedClientTestHooks.Str(json, "reason"); };
        var kick = new HttpRequestMessage(HttpMethod.Delete, "http://127.0.0.1:8090/v1/rooms/" + ManagedClientTestHooks.Str(gone.JoinedJson, "room") + "/players/" + gone.Actor + "?reason=idle%20too%20long");
        kick.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        if (!(await h.SendAsync(kick)).IsSuccessStatusCode) throw new Exception("kick refused");
        until = DateTime.UtcNow.AddSeconds(3); while (DateTime.UtcNow < until && gone.JoinedJson != null) { gone.Poll(); await Task.Delay(5); }
        if (gone.Actor != -1 || gone.JoinedJson != null || gone.RoomMode != null || gone.Seed != null) throw new Exception("kick: the client still describes the room");
        if (why != "idle too long") throw new Exception("kick: the reason came as " + why);
        // a late joiner gets the cached events, and the chat history once it joins the channel
        var chatA = new ChatClient(ra); await chatA.JoinAsync(1);
        ra.SendEvent(3, new byte[] { 7 }, Target.Others, reliable: true, cache: CacheOp.Add); chatA.Send(1, "hello");
        until = DateTime.UtcNow.AddMilliseconds(300); while (DateTime.UtcNow < until) { ra.Poll(); await Task.Delay(5); }
        var rc = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-rc"));
        int cached = 0, history = 0; rc.OnEvent += f => { if (f.Code == 3 && f.Cached) cached++; };
        var chatC = new ChatClient(rc); chatC.OnMessage += m => { if (m.Text == "hello" && m.Cached) history++; };
        await rc.JoinAsync(room + "-second"); await chatC.JoinAsync(1);
        Console.WriteLine($"late joiner: cached events={cached}/1 chat history={history}/1");
        ra.Dispose(); rb.Dispose(); rc.Dispose();
        // over a slow link (NET_SLOW_HTTP, see README): a client that left a room joins another one
        bool rejoined = true, toldApart = true; var slow = Environment.GetEnvironmentVariable("NET_SLOW_HTTP");
        if (slow != null)
        {
            var sf = await ManagedClient.ConnectAsync(slow, await Grant(h, key, "cs-sf"));
            var sg = await ManagedClient.ConnectAsync(slow, await Grant(h, key, "cs-sg"));
            int errors = 0, next = 0;
            sf.OnControl += json => { if (json.Contains("\"t\":\"error\"")) errors++; };
            sg.OnEvent += f => { if (f.Code == 5) next++; };
            await sf.JoinOrCreateAsync(room + "-slow", "relay", 4); await sg.JoinOrCreateAsync(room + "-slow", "relay", 4);
            // events on their way to `sf` when it leaves must not make it answer the room it has left
            for (int i = 0; i < 30; i++) { sg.SendEvent(4, new byte[] { 9 }, Target.Others, reliable: true); sf.Poll(); sg.Poll(); await Task.Delay(10); }
            await sf.LeaveAsync();
            for (int i = 0; i < 30; i++) { sg.SendEvent(4, new byte[] { 9 }, Target.Others, reliable: true); sf.Poll(); sg.Poll(); await Task.Delay(10); }
            try
            {
                await sf.JoinOrCreateAsync(room + "-slow2", "relay", 4); await sg.LeaveAsync(); await sg.JoinAsync(room + "-slow2");
                for (int i = 0; i < 5; i++) sf.SendEvent(5, new byte[] { (byte)i }, Target.Others, reliable: true);
                until = DateTime.UtcNow.AddSeconds(2); while (DateTime.UtcNow < until && next < 5) { sf.Poll(); sg.Poll(); await Task.Delay(5); }
            }
            catch (Exception e) { Console.WriteLine($"slow link: {e.Message}"); errors++; }
            Console.WriteLine($"slow link: errors after leave={errors}, reliable events in the next room={next}/5");
            rejoined = errors == 0 && next == 5;
            // two lookups in flight, both sent before either is answered: the node answers one a second, and
            // its refusal of the second is not the first one's
            var dust = sf.RoomsAsync("map = 'dust'"); var nuke = sf.RoomsAsync("map = 'nuke'"); string list = null, refusal = null;
            try { list = await dust; } catch (NetException e) { list = e.Message; }
            try { await nuke; } catch (NetException e) { refusal = e.Message; }
            toldApart = ManagedClientTestHooks.Str(list, "t") == "rooms" && refusal != null && refusal.StartsWith("conflict");
            Console.WriteLine($"slow link, two lookups in flight: first={(ManagedClientTestHooks.Str(list, "t") ?? list)}, second={refusal ?? "answered"} -> {(toldApart ? "ok" : "FAIL")}");
            sf.Dispose(); sg.Dispose();
        }
        else rejoined = Skip("slow link: skipped (NET_SLOW_HTTP not set)");
        // a node whose hello does not list the unsequenced channel (NET_PLAIN_HTTP, see README): events for
        // that channel go on the sequenced one, from the managed client and from the one over the native core
        bool fellBack = true; var plain = Environment.GetEnvironmentVariable("NET_PLAIN_HTTP");
        if (plain != null)
        {
            var listener = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-plain-l"));
            await listener.JoinOrCreateAsync(room + "-plain", "relay", 4);
            var arrived = new Dictionary<byte, List<Channel>>();
            listener.OnData += (channel, _, payload) =>
            {
                if (EventFrame.TryDecode(payload, out var f) && arrived.TryGetValue(f.Code, out var channels)) channels.Add(channel);
            };
            async Task<string> Sent(byte code, Action<byte> send, Action poll)
            {
                arrived[code] = new List<Channel>();
                for (int i = 0; i < 10; i++) { send(code); poll(); listener.Poll(); await Task.Delay(16); }
                until = DateTime.UtcNow.AddSeconds(2); while (DateTime.UtcNow < until && arrived[code].Count < 10) { poll(); listener.Poll(); await Task.Delay(5); }
                return $"{arrived[code].Count}/10 on channel {string.Join(",", arrived[code].Distinct().Select(c => (int)c))}";
            }
            var one = new byte[] { 1 };
            var managed = await ManagedClient.ConnectAsync(plain, await Grant(h, key, "cs-plain-m"));
            await managed.JoinAsync(room + "-plain");
            string viaManaged = await Sent(11, code => { managed.SendEventOn(Channel.UnreliableUnsequenced, code, one); managed.Flush(); }, () => managed.Poll());
            managed.Dispose();
            string viaCore, viaCoreDirect;
            try
            {
                using var core = Client.Connect(plain, await Grant(h, key, "cs-plain-c"));
                if (core.PlayerId != "cs-plain-c" || core.PlayerName != "cs-plain-c") throw new Exception("native player: id " + core.PlayerId + ", name " + core.PlayerName);
                core.Join(room + "-plain");
                viaCore = await Sent(12, code => { core.SendEventOn(Channel.UnreliableUnsequenced, code, one); core.Flush(); }, () => core.Poll());
                using var direct = Client.Connect("http://127.0.0.1:8090", await Grant(h, key, "cs-plain-d"));
                direct.Join(room + "-plain");
                viaCoreDirect = await Sent(13, code => { direct.SendEventOn(Channel.UnreliableUnsequenced, code, one); direct.Flush(); }, () => direct.Poll());
            }
            catch (DllNotFoundException) { viaCore = viaCoreDirect = "skipped (libaurix_net is not on the library path)"; }
            Console.WriteLine($"node without the unsequenced channel: managed={viaManaged}, native core={viaCore}; native core, node with it={viaCoreDirect}");
            fellBack = viaManaged == "10/10 on channel 0" && ((viaCore.StartsWith("skipped") && !Required) || (viaCore == "10/10 on channel 0" && viaCoreDirect == "10/10 on channel 4"));
            listener.Dispose();
        }
        else fellBack = Skip("node without the unsequenced channel: skipped (NET_PLAIN_HTTP not set)");
        // a control channel that closes on `auth` (NET_CUT_HTTP, see README): ConnectAsync fails as it closes, not at its timeout
        bool failedFast = true; var cut = Environment.GetEnvironmentVariable("NET_CUT_HTTP");
        if (cut != null)
        {
            var clock = System.Diagnostics.Stopwatch.StartNew(); string failure = "none";
            try { (await ManagedClient.ConnectAsync(cut, await Grant(h, key, "cs-cut"))).Dispose(); }
            catch (NetException error) { failure = error.Message; }
            failedFast = failure == "control channel closed" && clock.ElapsedMilliseconds < 5000;
            Console.WriteLine($"closed control channel: connect failed after {clock.ElapsedMilliseconds} ms: {failure} -> {(failedFast ? "ok" : "FAIL")}");
        }
        else failedFast = Skip("closed control channel: skipped (NET_CUT_HTTP not set)");
        // outside a room, also one the node closed, and on a connection that is gone a send is refused,
        // by the managed client and by the native core
        bool refused;
        using (var relay = new Relay())
        {
            string Refusal(Action send) { try { send(); return "sent"; } catch (NetException error) { return error.Message; } }
            var one = new byte[] { 1 };
            var managed = await ManagedClient.ConnectAsync(relay.Url, await Grant(h, key, "cs-lost-m"));
            bool managedClosed = false; managed.OnClosed += _ => managedClosed = true;
            Action viaManaged = () => managed.SendEvent(1, one, Target.Others, reliable: true);
            string outside = Refusal(viaManaged);
            await managed.JoinOrCreateAsync(room + "-lost", "relay", 4);
            string inside = Refusal(viaManaged);
            Client core = null; bool coreClosed = false, coreOut = false; Action viaCore = null;
            string coreOutside = "skipped", coreInside = "skipped", coreTaken = "skipped", coreBack = "skipped", coreGone = "skipped";
            try
            {
                core = Client.Connect(relay.Url, await Grant(h, key, "cs-lost-c"));
                core.OnClosed += _ => coreClosed = true;
                core.OnControl += json => coreOut |= ManagedClientTestHooks.Str(json, "t") == "left";
                viaCore = () => core.SendEvent(1, one, Target.Others, reliable: true);
                coreOutside = Refusal(viaCore);
                core.Join(room + "-lost");
                // the native client describes the room as the node did at the join: its size and who was in it, itself too
                if (ManagedClientTestHooks.Num(core.JoinedJson, "max_players") != 4 || !core.JoinedJson.Contains("\"player_id\":\"cs-lost-m\"") || !core.JoinedJson.Contains("\"player_id\":\"cs-lost-c\"")) throw new Exception("joined: the native client does not describe the room: " + core.JoinedJson);
                coreInside = Refusal(viaCore);
                core.Leave();
                if (core.Actor != -1 || core.JoinedJson != null) throw new Exception("leave: the native client still describes the room");
                // a room the native client creates is found by the props it publishes
                core.JoinOrCreate(room + "-map", RoomMode.Relay, 4, "{\"map\":\"" + room + "\"}", null, new[] { "map" });
                await managed.LeaveAsync();
                await managed.JoinRandomAsync(null, "map = '" + room + "'");
                if (ManagedClientTestHooks.Str(managed.JoinedJson, "name") != room + "-map") throw new Exception("random join: not the room the native client published");
                core.Leave(); await managed.LeaveAsync();
                // a hidden room is not offered to a random join and is entered by its name; a closed room lets its creator in and nobody else
                async Task<string> Refused(Func<Task> join) { try { await join(); return "joined"; } catch (NetException error) { return error.Message; } }
                using (var hider = Client.Connect("http://127.0.0.1:8090", await Grant(h, key, "cs-hider")))
                using (var closer = Client.Connect("http://127.0.0.1:8090", await Grant(h, key, "cs-closer")))
                {
                    hider.JoinOrCreate(room + "-hidden", RoomMode.Relay, 4, "{\"map\":\"" + room + "-hidden\"}", null, new[] { "map" }, visible: false);
                    closer.JoinOrCreate(room + "-closed", RoomMode.Relay, 4, open: false);
                    string offered = await Refused(() => managed.JoinRandomAsync(null, "map = '" + room + "-hidden'"));
                    string closed = await Refused(() => managed.JoinAsync(room + "-closed")) + " / " + await Refused(() => managed.JoinOrCreateAsync(room + "-closed", "relay", 4));
                    await managed.JoinAsync(room + "-hidden");
                    await managed.LeaveAsync();
                    await managed.JoinOrCreateAsync(room + "-hidden-m", "relay", 4, "{\"map\":\"" + room + "-hidden-m\"}", null, new[] { "map" }, visible: false);
                    string offeredCore = Refusal(() => core.JoinRandom(null, "map = '" + room + "-hidden-m'"));
                    await managed.LeaveAsync();
                    await managed.JoinOrCreateAsync(room + "-closed-m", "relay", 4, open: false);
                    string closedCore = Refusal(() => core.Join(room + "-closed-m")) + " / " + Refusal(() => core.JoinOrCreate(room + "-closed-m", RoomMode.Relay, 4));
                    await managed.LeaveAsync();
                    Console.WriteLine($"hidden room: random join '{offered}' and '{offeredCore}'; closed room: '{closed}' and '{closedCore}'");
                    if (!offered.Contains("not_found") || !offeredCore.Contains("not_found")) throw new Exception("hidden room: a random join was offered it");
                    if (closed.Split("room_closed").Length != 3 || closedCore.Split("room_closed").Length != 3) throw new Exception("closed room: a join was not refused as room_closed");
                }
                await managed.JoinOrCreateAsync(room + "-lost", "relay", 4);
                core.Join(room + "-lost");
            }
            catch (DllNotFoundException) { core = null; }
            var close = new HttpRequestMessage(HttpMethod.Delete, "http://127.0.0.1:8090/v1/rooms/" + ManagedClientTestHooks.Str(managed.JoinedJson, "room"));
            close.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
            if (!(await h.SendAsync(close)).IsSuccessStatusCode) throw new Exception("close refused");
            var limit = DateTime.UtcNow.AddSeconds(5);
            while (DateTime.UtcNow < limit && (managed.JoinedJson != null || (core != null && !coreOut))) { managed.Poll(); core?.Poll(); await Task.Delay(5); }
            if (core != null && (core.Actor != -1 || core.JoinedJson != null)) throw new Exception("close: the native client still describes the room");
            string taken = Refusal(viaManaged);
            await managed.JoinOrCreateAsync(room + "-lost", "relay", 4);
            string back = Refusal(viaManaged);
            if (core != null) { coreTaken = Refusal(viaCore); core.Join(room + "-lost"); coreBack = Refusal(viaCore); }
            relay.Dispose();
            limit = DateTime.UtcNow.AddSeconds(5);
            while (DateTime.UtcNow < limit && !(managedClosed && (core == null || coreClosed))) { managed.Poll(); core?.Poll(); await Task.Delay(5); }
            string lost = Refusal(viaManaged);
            if (core != null) coreGone = Refusal(viaCore);
            managed.Dispose(); core?.Dispose();
            Console.WriteLine($"send outside a room / in it / out of a closed room / in the next room / on a closed connection: managed={outside} / {inside} / {taken} / {back} / {lost}, native core={coreOutside} / {coreInside} / {coreTaken} / {coreBack} / {coreGone}");
            string expected = "disconnected sent disconnected sent disconnected";
            refused = $"{outside} {inside} {taken} {back} {lost}" == expected && ((core == null && !Required) || $"{coreOutside} {coreInside} {coreTaken} {coreBack} {coreGone}" == expected);
        }
        // NetRunner over the slow link: every frame the owner sends reaches the room, the last move among them,
        // and it sends one a tick: the moves are a step of the stub's clock apart, so `moves * step * TickHz` of them
        bool runnerOk = true;
        if (slow != null)
        {
            var runner = new NetRunner { Settings = new NetSettings { ServerUrl = slow } };
            await runner.ConnectAsync(await Grant(h, key, "cs-runner"));
            await runner.JoinOrCreateAsync(room + "-runner", RoomMode.Sync);
            var transformDef = runner.Schema.ByName("Transform");
            var watched = new SyncWorld(runner.Schema); var reached = new HashSet<int>();
            var watcher = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-watcher"));
            watcher.OnData += (ch, t, p) => { var ack = watched.Receive(p); if (ack != null) watcher.SendData(Channel.Unreliable, ack); };
            watched.OnChange += entity => { if (entity.Owner == runner.Actor && entity.Get(transformDef) is ComponentValue value) reached.Add((int)Math.Round(value.Fields[0].X)); };
            await watcher.JoinAsync(room + "-runner", ManagedClientTestHooks.Str(runner.Client.JoinedJson, "schema_hash"));
            // the stubs have no prefabs to instantiate, so the owner's view is bound by hand
            NetTransform mine = null;
            runner.World.OnSpawn += entity =>
            {
                if (entity.Owner != runner.Actor) return;
                var view = new NetView { Runner = runner, Entity = entity, EntityId = entity.Id, Owner = entity.Owner };
                mine = new NetTransform(); view.Behaviours.Add(mine); mine.Bind(view); runner.Views[entity.Id] = view;
            };
            var update = typeof(NetRunner).GetMethod("Update", BindingFlags.Instance | BindingFlags.NonPublic);
            var lateUpdate = typeof(NetRunner).GetMethod("LateUpdate", BindingFlags.Instance | BindingFlags.NonPublic);
            int moves = 0; var written = new HashSet<int>(); uint last = runner.LastInputTick;
            for (int step = 0; step < 420; step++)
            {
                if (step >= 120 && step < 360 && mine != null) mine.Position = new Vector3(++moves, 1, -2);
                update.Invoke(runner, null); lateUpdate.Invoke(runner, null);
                if (runner.LastInputTick != last) { last = runner.LastInputTick; if (moves > 0) written.Add(moves); }
                watcher.Flush(); watcher.Poll(); await Task.Delay(16);
            }
            reached.Remove(0);
            double ticks = Math.Min(moves, moves * Time.unscaledDeltaTime * runner.Settings.TickHz);
            Console.WriteLine($"slow link, NetRunner: owner moves={moves}, sent={written.Count} (ticks in that time={ticks:F1}), reached the room={reached.Count}");
            runnerOk = moves > 0 && Math.Abs(written.Count - ticks) <= 1 && reached.Contains(moves) && reached.SetEquals(written);
            runner.Client.Dispose(); watcher.Dispose();
        }
        // the node gives up a link whose acknowledgements stop reaching it and says so once, on the
        // client's next frame: that answer ends the data path of a client that sent nothing reliable
        bool givenUpOk;
        using (var gag = new GagRelay())
        {
            var quiet = await ManagedClient.ConnectAsync(gag.Url, await Grant(h, key, "cs-gag-a"));
            var talker = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-gag-b"));
            var answers = new List<string>(); var reasons = new List<string>();
            quiet.OnControl += json => { if (ManagedClientTestHooks.Str(json, "t") == "error") answers.Add(ManagedClientTestHooks.Str(json, "code")); };
            quiet.OnClosed += reason => reasons.Add(reason);
            string gagRoom = "cs-gag-" + Environment.ProcessId;
            await quiet.JoinOrCreateAsync(gagRoom, "relay", 4); await talker.JoinAsync(gagRoom);
            gag.Gagged = true;
            talker.SendEvent(1, new byte[8], Target.Others, reliable: true); talker.Flush();
            var clock = System.Diagnostics.Stopwatch.StartNew();
            while (clock.ElapsedMilliseconds < 17000) { quiet.Poll(); talker.Poll(); await Task.Delay(10); }
            bool aliveBefore = reasons.Count == 0;
            gag.Gagged = false;
            for (int i = 0; i < 5; i++) quiet.SendEvent(2, new byte[8], Target.Others, reliable: false);
            quiet.Flush();
            for (int i = 0; i < 120 && reasons.Count == 0; i++) { quiet.Poll(); await Task.Delay(25); }
            for (int i = 0; i < 12; i++) { quiet.Poll(); await Task.Delay(25); }
            bool sendRefused = false;
            try { quiet.SendEvent(2, new byte[8], Target.Others, reliable: true); } catch (NetException) { sendRefused = true; }
            Console.WriteLine($"given-up link: alive before={aliveBefore}, answers=[{string.Join(",", answers)}], closed=[{string.Join(",", reasons)}], send refused={sendRefused}");
            givenUpOk = aliveBefore && answers.SequenceEqual(new[] { "transport" }) && reasons.SequenceEqual(new[] { "link dead" }) && sendRefused;
            quiet.Dispose(); talker.Dispose();
        }
        // a game thread that is blocked for a while (a scene load) keeps its link: the client
        // reads, acknowledges and resends without it
        bool busyOk;
        {
            var stillGrant = await Grant(h, key, "cs-busy-a");
            var talker = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-busy-b"));
            var busyErrors = new List<string>(); var busyReasons = new List<string>(); int busyGot = 0, busyBack = 0, busySent = 0;
            talker.OnEvent += frame => { if (frame.Code == 2) Interlocked.Increment(ref busyBack); };
            string busyRoom = "cs-busy-" + Environment.ProcessId;
            var inRoom = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            var bothIn = new ManualResetEventSlim(); var talked = new ManualResetEventSlim();
            var game = new Thread(() =>
            {
                var thread = new GameThread(); SynchronizationContext.SetSynchronizationContext(thread);
                var still = thread.Run(() => ManagedClient.ConnectAsync("http://127.0.0.1:8090", stillGrant));
                still.OnControl += json => { if (ManagedClientTestHooks.Str(json, "t") == "error") busyErrors.Add(ManagedClientTestHooks.Str(json, "code")); };
                still.OnClosed += reason => busyReasons.Add(reason);
                still.OnEvent += frame => { if (frame.Code == 1) busyGot++; };
                thread.Run(() => still.JoinOrCreateAsync(busyRoom, "relay", 4));
                inRoom.SetResult(true); bothIn.Wait();
                still.SendEvent(2, new byte[8], Target.Others, reliable: true);
                talked.Wait();
                for (int i = 0; i < 80 && (busyGot < Volatile.Read(ref busySent) || Volatile.Read(ref busyBack) == 0) && busyReasons.Count == 0; i++) { thread.Pump(); still.Poll(); Thread.Sleep(25); }
                still.Dispose();
            });
            game.Start();
            await inRoom.Task; await talker.JoinAsync(busyRoom); bothIn.Set();
            var busyClock = System.Diagnostics.Stopwatch.StartNew();
            while (busyClock.ElapsedMilliseconds < 20000)
            {
                if (busyClock.ElapsedMilliseconds >= busySent * 500) { talker.SendEvent(1, new byte[8], Target.Others, reliable: true); Interlocked.Increment(ref busySent); }
                talker.Poll(); await Task.Delay(10);
            }
            talked.Set();
            while (game.IsAlive) { talker.Poll(); await Task.Delay(10); }
            Console.WriteLine($"busy game: sent={busySent}, got={busyGot}, reached={busyBack}, closed=[{string.Join(",", busyReasons)}], errors=[{string.Join(",", busyErrors)}]");
            busyOk = busyGot == busySent && busyBack == 1 && busyReasons.Count == 0 && busyErrors.Count == 0;
            talker.Dispose();
        }
        // a send the game started and the socket could not take at once does not hold the link
        // until the game thread runs again
        bool stallOk;
        using (var stall = new GagRelay())
        {
            stall.DropOver = 512;
            var stillGrant = await Grant(h, key, "cs-stall-a");
            var talker = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-stall-b"));
            var stallReasons = new List<string>(); int stallGot = 0, stallSent = 0;
            string stallRoom = "cs-stall-" + Environment.ProcessId;
            var inRoom = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            var bothIn = new ManualResetEventSlim(); var flooded = new ManualResetEventSlim(); var talked = new ManualResetEventSlim();
            var game = new Thread(() =>
            {
                var thread = new GameThread(); SynchronizationContext.SetSynchronizationContext(thread);
                var still = thread.Run(() => ManagedClient.ConnectAsync(stall.Url, stillGrant));
                still.OnClosed += reason => stallReasons.Add(reason);
                still.OnEvent += frame => { if (frame.Code == 1) stallGot++; };
                thread.Run(() => still.JoinOrCreateAsync(stallRoom, "relay", 4));
                inRoom.SetResult(true); bothIn.Wait();
                stall.Stalled = true;
                for (int i = 0; i < 30000; i++) { still.SendEvent(3, new byte[1000], Target.Others, reliable: false); if (i % 20 == 19) still.Flush(); }
                flooded.Set(); talked.Wait();
                for (int i = 0; i < 80 && stallGot < Volatile.Read(ref stallSent) && stallReasons.Count == 0; i++) { thread.Pump(); still.Poll(); Thread.Sleep(25); }
                still.Dispose();
            });
            game.Start();
            await inRoom.Task; await talker.JoinAsync(stallRoom); bothIn.Set();
            while (!flooded.IsSet) { talker.Poll(); await Task.Delay(10); }
            stall.Stalled = false;
            var stallClock = System.Diagnostics.Stopwatch.StartNew();
            while (stallClock.ElapsedMilliseconds < 20000)
            {
                if (stallClock.ElapsedMilliseconds >= stallSent * 500) { talker.SendEvent(1, new byte[8], Target.Others, reliable: true); Interlocked.Increment(ref stallSent); }
                talker.Poll(); await Task.Delay(10);
            }
            talked.Set();
            while (game.IsAlive) { talker.Poll(); await Task.Delay(10); }
            Console.WriteLine($"stalled send: sent={stallSent}, got={stallGot}, closed=[{string.Join(",", stallReasons)}]");
            stallOk = stallGot == stallSent && stallReasons.Count == 0;
            talker.Dispose();
        }
        bool ok = wayBack && stallOk && busyOk && givenUpOk && refused && lsOk && schemaOk && spawnOk && tailOk && rel == 1 && unrel == 10 && big == 1 && bigLen == 5000 && deltas > 60 && again == 5 && cached == 1 && history == 1 && unsequenced == 10 && rejoined && toldApart && fellBack && failedFast && runnerOk;
        Console.WriteLine(ok ? "MANAGED OK" : "MANAGED FAIL");
        return ok ? 0 : 1;
    }
}
