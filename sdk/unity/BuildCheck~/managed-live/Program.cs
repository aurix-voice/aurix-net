using System;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net;
using Aurix.Net.Managed;

class P
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
    static async Task<int> Main()
    {
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        var h = new HttpClient();
        var room = "cs-" + DateTime.UtcNow.Ticks;
        var a = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-a"));
        var b = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-b"));
        int rel = 0, unrel = 0, big = 0, bigLen = 0; string ctl = "";
        b.OnEvent += f => { if (f.Code == 1) rel++; else if (f.Code == 2) unrel++; else if (f.Code == 3) { big++; bigLen = f.Payload.Count; } };
        b.OnControl += j => { if (j.Contains("player_joined")) ctl += "J"; };
        int aa = await a.JoinOrCreateAsync(room, "relay", 8);
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
        bool ok = rel == 1 && unrel == 10 && big == 1 && bigLen == 5000 && deltas > 60;
        Console.WriteLine(ok ? "MANAGED OK" : "MANAGED FAIL");
        return ok ? 0 : 1;
    }
}
