// Live rehost check: joins a relay room on node A, waits for OnRehost (A got SIGTERM), RejoinAsync
// via node B with the same grant, then a second client on B must still reach it.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net;
using Aurix.Net.Managed;

class P
{
    const string A = "http://127.0.0.1:8090", B = "http://127.0.0.1:8091";
    static async Task<string> Grant(HttpClient h, string http, string key, string pid)
    {
        var req = new HttpRequestMessage(HttpMethod.Post, http + "/v1/grants");
        req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        req.Content = new StringContent("{\"player_id\":\"" + pid + "\",\"name\":\"" + pid + "\"}", Encoding.UTF8, "application/json");
        var r = await h.SendAsync(req); var body = await r.Content.ReadAsStringAsync();
        if (!r.IsSuccessStatusCode) throw new Exception(body);
        int i = body.IndexOf("\"grant\":\"") + 9; return body.Substring(i, body.IndexOf('"', i) - i);
    }
    static async Task<int> Main(string[] args)
    {
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        var room = args.Length > 0 ? args[0] : "cs-rehost-" + DateTime.UtcNow.Ticks;
        var h = new HttpClient();
        var a = await ManagedClient.ConnectAsync(A, await Grant(h, A, key, "cs-rehost-a"));
        int actor1 = await a.JoinOrCreateAsync(room, "relay", 8);
        Console.WriteLine($"cs: joined {room} actor={actor1} on A");
        // a client that a random join brought into a room: the room is full with it, and another room it
        // could be matched into stands open beside it
        string tag = "{\"tag\":\"" + room + "-cs\"}";
        var host = await ManagedClient.ConnectAsync(A, await Grant(h, A, key, "cs-rehost-h"));
        await host.JoinOrCreateAsync(room + "-cs-full", "relay", 2, tag);
        var r = await ManagedClient.ConnectAsync(A, await Grant(h, A, key, "cs-rehost-r"));
        int actorR = await r.JoinRandomAsync(tag);
        string roomR = ManagedClientTestHooks.Str(r.JoinedJson, "name");
        var beside = await ManagedClient.ConnectAsync(A, await Grant(h, A, key, "cs-rehost-o"));
        await beside.JoinOrCreateAsync(room + "-cs-open", "relay", 8, tag);
        bool moved = false; r.OnRehost += () => moved = true;
        Console.WriteLine($"cs: a random join led to {roomR} actor={actorR}");
        bool rehost = false; int frames = 0;
        a.OnRehost += () => rehost = true;
        a.OnData += (ch, t, p) => frames++;
        // a reliable event the room did not get is handed back: none is lost without a word
        var listener = await ManagedClient.ConnectAsync(A, await Grant(h, A, key, "cs-rehost-l"));
        await listener.JoinAsync(room);
        var reached = new HashSet<int>(); var handedBack = new HashSet<int>(); int sent = 0;
        listener.OnEvent += frame => { if (frame.Code == 3) reached.Add(BitConverter.ToInt32(frame.Payload.ToArray(), 0)); };
        a.OnUndelivered += events => { foreach (var back in events) handedBack.Add(BitConverter.ToInt32(back.Payload, 0)); };
        var deadline = DateTime.UtcNow.AddSeconds(60);
        while (!rehost && DateTime.UtcNow < deadline)
        {
            a.SendEvent(1, new byte[] { 1 }, Target.Others, reliable: false);
            a.SendEvent(3, BitConverter.GetBytes(sent++), Target.Others, reliable: true); a.Flush(); a.Poll();
            r.Poll(); host.Poll(); beside.Poll(); listener.Poll();
            await Task.Delay(20);
        }
        if (!rehost) { Console.WriteLine("CS_REHOST_FAIL no rehost within 60 s"); return 1; }
        Console.WriteLine($"cs: got rehost after {frames} data frames");
        for (int i = 0; i < 25; i++) { listener.Poll(); await Task.Delay(20); }
        int neither = Enumerable.Range(0, sent).Count(n => !reached.Contains(n) && !handedBack.Contains(n));
        Console.WriteLine($"cs: reliable events at rehost: sent {sent}, the room got {reached.Count}, handed back {handedBack.Count}, neither {neither}, both {handedBack.Count(reached.Contains)}");
        if (a.Actor != -1 || a.JoinedJson != null) { Console.WriteLine("CS_REHOST_FAIL still describes the room the node handed off"); return 1; }
        var a2 = await a.RejoinAsync(B);
        Console.WriteLine($"cs: rejoined actor={a2.Actor} (same actor: {a2.Actor == actor1})");
        deadline = DateTime.UtcNow.AddSeconds(10);
        while (!moved && DateTime.UtcNow < deadline) { r.Poll(); await Task.Delay(20); }
        string back;
        try
        {
            var r2 = await r.RejoinAsync(B);
            back = ManagedClientTestHooks.Str(r2.JoinedJson, "name") + " actor=" + r2.Actor;
            r2.Dispose();
        }
        catch (NetException refusal) { back = "refused: " + refusal.Message; }
        bool returned = back == roomR + " actor=" + actorR;
        Console.WriteLine($"cs: after a random join the rejoin led to {back} (same room and actor: {returned})");
        string got = null;
        a2.OnEvent += f => { if (f.Code == 7) got = Encoding.UTF8.GetString(f.Payload.Array, f.Payload.Offset, f.Payload.Count); };
        var b = await ManagedClient.ConnectAsync(B, await Grant(h, B, key, "cs-rehost-b"));
        await b.JoinAsync(room);
        b.SendEvent(7, Encoding.UTF8.GetBytes("after-rehost"), Target.Others, reliable: true); b.Flush();
        deadline = DateTime.UtcNow.AddSeconds(3);
        while (got == null && DateTime.UtcNow < deadline) { a2.Poll(); b.Poll(); await Task.Delay(5); }
        bool ok = got == "after-rehost" && a2.Actor == actor1 && returned && neither == 0;
        Console.WriteLine(ok ? "CS_REHOST_OK" : $"CS_REHOST_FAIL got={got}");
        a2.Dispose(); b.Dispose(); host.Dispose(); beside.Dispose(); listener.Dispose();
        return ok ? 0 : 1;
    }
}
