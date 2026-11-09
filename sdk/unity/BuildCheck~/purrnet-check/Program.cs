// Compile check + live test of the PurrNet transport adapter over a local node (skips the live part without NET_API_KEY).
using System;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net.PurrNet;
using PurrNet.Transports;

class Program
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

    static AurixNetTransport Make(HttpClient h, string key, string pid, string room)
    {
        var t = new AurixNetTransport { roomName = room, GrantProvider = () => Grant(h, key, pid) };
        return t;
    }

    static async Task Pump(params AurixNetTransport[] ts) { var until = DateTime.UtcNow.AddMilliseconds(1500); while (DateTime.UtcNow < until) { foreach (var t in ts) { t.ReceiveMessages(0); t.SendMessages(0); } await Task.Delay(5); } }

    static async Task<int> Main()
    {
        Console.WriteLine("PURRNET ADAPTER OK " + typeof(AurixNetTransport).FullName);
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        if (string.IsNullOrEmpty(key)) { Console.WriteLine("NET_API_KEY unset: live test skipped"); return 0; }
        var h = new HttpClient(); var room = "purr-" + DateTime.UtcNow.Ticks;
        var host = Make(h, key, "purr-host", room); var c1 = Make(h, key, "purr-c1", room); var c2 = Make(h, key, "purr-c2", room);

        string log = "";
        host.onConnected += (c, s) => log += $"H+{c.connectionId} ";
        host.onDisconnected += (c, r, s) => log += $"H-{c.connectionId}:{r} ";
        host.onDataReceived += (c, d, s) => { log += $"Hrx{c.connectionId}:{Encoding.UTF8.GetString(d.data, d.offset, d.length)} "; host.SendToClient(c, new ByteData(Encoding.UTF8.GetBytes("pong"), 0, 4), Channel.Unreliable); };
        foreach (var (c, n) in new[] { (c1, "1"), (c2, "2") })
        {
            c.onConnected += (cc, s) => log += $"C{n}+ ";
            c.onDisconnected += (cc, r, s) => log += $"C{n}-:{r} ";
            c.onDataReceived += (cc, d, s) => log += d.length > 64 ? $"C{n}rx:{d.length}B " : $"C{n}rx:{Encoding.UTF8.GetString(d.data, d.offset, d.length)} ";
        }

        host.Listen(0); await Pump(host);
        if (host.listenerState != ConnectionState.Connected) { Console.WriteLine("FAIL host not listening"); return 1; }
        c1.Connect(null, 0); c2.Connect(null, 0); await Pump(host, c1, c2);
        Console.WriteLine($"connections on host: {host.connections.Count}/2  c1={c1.clientState} c2={c2.clientState}");
        c1.SendToServer(new ByteData(Encoding.UTF8.GetBytes("ping1"), 0, 5), Channel.ReliableOrdered);
        c2.SendToServer(new ByteData(Encoding.UTF8.GetBytes("ping2"), 0, 5), Channel.ReliableOrdered);
        await Pump(host, c1, c2);
        var big = new byte[4000]; host.SendToClient(host.connections[0], new ByteData(big, 0, big.Length), Channel.ReliableOrdered); await Pump(host, c1, c2);
        host.CloseConnection(host.connections[1]); await Pump(host, c1, c2);
        host.StopListening(); await Pump(host, c1, c2);
        Console.WriteLine("log: " + log);
        bool ok = log.Contains("H+") && log.Contains("C1+") && log.Contains("C2+") && log.Contains("Hrx") && log.Contains("ping1") && log.Contains("ping2")
                  && log.Contains("C1rx:pong") && log.Contains("C2rx:pong") && log.Contains("rx:4000B") && log.Contains("ServerRequest") && host.connections.Count == 0;
        Console.WriteLine(ok ? "PURRNET LIVE OK" : "PURRNET LIVE FAIL");
        return ok ? 0 : 1;
    }
}
