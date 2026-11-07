// Live rehost check: joins a relay room on node A, waits for OnRehost (A got SIGTERM), RejoinAsync
// via node B with the same grant, then a second client on B must still reach it.
using System;
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
        bool rehost = false; int frames = 0;
        a.OnRehost += () => rehost = true;
        a.OnData += (ch, t, p) => frames++;
        var deadline = DateTime.UtcNow.AddSeconds(60);
        while (!rehost && DateTime.UtcNow < deadline)
        {
            a.SendEvent(1, new byte[] { 1 }, Target.Others, reliable: false); a.Flush(); a.Poll();
            await Task.Delay(50);
        }
        if (!rehost) { Console.WriteLine("CS_REHOST_FAIL no rehost within 60 s"); return 1; }
        Console.WriteLine($"cs: got rehost after {frames} data frames");
        var a2 = await a.RejoinAsync(B);
        Console.WriteLine($"cs: rejoined actor={a2.Actor} (same actor: {a2.Actor == actor1})");
        string got = null;
        a2.OnEvent += f => { if (f.Code == 7) got = Encoding.UTF8.GetString(f.Payload.Array, f.Payload.Offset, f.Payload.Count); };
        var b = await ManagedClient.ConnectAsync(B, await Grant(h, B, key, "cs-rehost-b"));
        await b.JoinAsync(room);
        b.SendEvent(7, Encoding.UTF8.GetBytes("after-rehost"), Target.Others, reliable: true); b.Flush();
        deadline = DateTime.UtcNow.AddSeconds(3);
        while (got == null && DateTime.UtcNow < deadline) { a2.Poll(); b.Poll(); await Task.Delay(5); }
        bool ok = got == "after-rehost" && a2.Actor == actor1;
        Console.WriteLine(ok ? "CS_REHOST_OK" : $"CS_REHOST_FAIL got={got}");
        a2.Dispose(); b.Dispose();
        return ok ? 0 : 1;
    }
}
