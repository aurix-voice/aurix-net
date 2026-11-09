// Compile check + live test of the PurrNet / Mirror / FishNet transports over a local node
// (the live part skips without NET_API_KEY). The same scenario runs through each adapter:
// host + two clients, ping/pong both ways, a 4000-byte reliable frame, a kick, host stop.
using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;

interface IDriver
{
    string Name { get; }
    void Listen(); void StopListening(); bool Listening { get; } int Peers { get; }
    void Connect(); bool Connected { get; }
    void SendToServer(byte[] data, bool reliable); void SendToClient(int conn, byte[] data, bool reliable); void Kick(int conn); int FirstPeer { get; } int LastPeer { get; }
    void Pump();
    Action<string> Log { get; set; }
}

sealed class PurrDriver : IDriver
{
    readonly Aurix.Net.PurrNet.AurixNetTransport t = new Aurix.Net.PurrNet.AurixNetTransport();
    public string Name => "PurrNet";
    public Action<string> Log { get; set; }
    public PurrDriver(string room, Func<Task<string>> grant, string tag)
    {
        t.roomName = room; t.GrantProvider = grant;
        t.onConnected += (c, s) => Log?.Invoke(s ? $"H+{c.connectionId}" : $"{tag}+");
        t.onDisconnected += (c, r, s) => Log?.Invoke(s ? $"H-{c.connectionId}:{r}" : $"{tag}-:{r}");
        t.onDataReceived += (c, d, s) => Log?.Invoke((s ? $"Hrx{c.connectionId}:" : $"{tag}rx:") + Program.Show(d.data, d.offset, d.length));
    }
    public void Listen() => t.Listen(0); public void StopListening() => t.StopListening();
    public bool Listening => t.listenerState == PurrNet.Transports.ConnectionState.Connected; public int Peers => t.connections.Count;
    public void Connect() => t.Connect(null, 0); public bool Connected => t.clientState == PurrNet.Transports.ConnectionState.Connected;
    public void SendToServer(byte[] d, bool r) => t.SendToServer(new PurrNet.Transports.ByteData(d, 0, d.Length), r ? PurrNet.Transports.Channel.ReliableOrdered : PurrNet.Transports.Channel.Unreliable);
    public void SendToClient(int c, byte[] d, bool r) => t.SendToClient(new PurrNet.Transports.Connection(c), new PurrNet.Transports.ByteData(d, 0, d.Length), r ? PurrNet.Transports.Channel.ReliableOrdered : PurrNet.Transports.Channel.Unreliable);
    public void Kick(int c) => t.CloseConnection(new PurrNet.Transports.Connection(c));
    public int FirstPeer => t.connections[0].connectionId; public int LastPeer => t.connections[t.connections.Count - 1].connectionId;
    public void Pump() { t.ReceiveMessages(0); t.SendMessages(0); }
}

sealed class MirrorDriver : IDriver
{
    readonly Aurix.Net.MirrorTransport.AurixNetMirrorTransport t = new Aurix.Net.MirrorTransport.AurixNetMirrorTransport();
    readonly List<int> peers = new List<int>();
    public string Name => "Mirror";
    public Action<string> Log { get; set; }
    public MirrorDriver(string room, Func<Task<string>> grant, string tag)
    {
        t.roomName = room; t.GrantProvider = grant;
        t.OnServerConnected = c => { peers.Add(c); Log?.Invoke($"H+{c}"); };
        t.OnServerDisconnected = c => { peers.Remove(c); Log?.Invoke($"H-{c}:ServerRequest"); };
        t.OnServerDataReceived = (c, d, ch) => Log?.Invoke($"Hrx{c}:" + Program.Show(d.Array, d.Offset, d.Count));
        t.OnClientConnected = () => Log?.Invoke($"{tag}+");
        t.OnClientDisconnected = () => Log?.Invoke($"{tag}-:{last}");
        t.OnClientError = (e, m) => last = e.ToString();
        t.OnClientDataReceived = (d, ch) => Log?.Invoke($"{tag}rx:" + Program.Show(d.Array, d.Offset, d.Count));
    }
    string last = "ClientRequest";
    public void Listen() => t.ServerStart(); public void StopListening() => t.ServerStop();
    public bool Listening => t.ServerActive(); public int Peers => peers.Count;
    public void Connect() => t.ClientConnect(""); public bool Connected => t.ClientConnected();
    public void SendToServer(byte[] d, bool r) => t.ClientSend(new ArraySegment<byte>(d), r ? 0 : 1);
    public void SendToClient(int c, byte[] d, bool r) => t.ServerSend(c, new ArraySegment<byte>(d), r ? 0 : 1);
    public void Kick(int c) => t.ServerDisconnect(c);
    public int FirstPeer => peers[0]; public int LastPeer => peers[peers.Count - 1];
    public void Pump() { t.ServerEarlyUpdate(); t.ClientEarlyUpdate(); t.ServerLateUpdate(); t.ClientLateUpdate(); }
}

sealed class FishDriver : IDriver
{
    readonly Aurix.Net.FishNetTransport.AurixNetFishNetTransport t = new Aurix.Net.FishNetTransport.AurixNetFishNetTransport();
    readonly List<int> peers = new List<int>();
    public string Name => "FishNet";
    public Action<string> Log { get; set; }
    public FishDriver(string room, Func<Task<string>> grant, string tag)
    {
        t.roomName = room; t.GrantProvider = grant; t.Initialize(null, 0);
        t.OnRemoteConnectionState += a => { if (a.ConnectionState == FishNet.Transporting.RemoteConnectionState.Started) { peers.Add(a.ConnectionId); Log?.Invoke($"H+{a.ConnectionId}"); } else { peers.Remove(a.ConnectionId); Log?.Invoke($"H-{a.ConnectionId}:ServerRequest"); } };
        t.OnServerReceivedData += a => Log?.Invoke($"Hrx{a.ConnectionId}:" + Program.Show(a.Data.Array, a.Data.Offset, a.Data.Count));
        t.OnClientConnectionState += a => { if (a.ConnectionState == FishNet.Transporting.LocalConnectionState.Started) Log?.Invoke($"{tag}+"); else if (a.ConnectionState == FishNet.Transporting.LocalConnectionState.Stopped) Log?.Invoke($"{tag}-:Stopped"); };
        t.OnClientReceivedData += a => Log?.Invoke($"{tag}rx:" + Program.Show(a.Data.Array, a.Data.Offset, a.Data.Count));
    }
    public void Listen() => t.StartConnection(true); public void StopListening() => t.StopConnection(true);
    public bool Listening => t.GetConnectionState(true) == FishNet.Transporting.LocalConnectionState.Started; public int Peers => peers.Count;
    public void Connect() => t.StartConnection(false); public bool Connected => t.GetConnectionState(false) == FishNet.Transporting.LocalConnectionState.Started;
    public void SendToServer(byte[] d, bool r) => t.SendToServer(r ? (byte)0 : (byte)1, new ArraySegment<byte>(d));
    public void SendToClient(int c, byte[] d, bool r) => t.SendToClient(r ? (byte)0 : (byte)1, new ArraySegment<byte>(d), c);
    public void Kick(int c) => t.StopConnection(c, true);
    public int FirstPeer => peers[0]; public int LastPeer => peers[peers.Count - 1];
    public void Pump() { t.IterateIncoming(true); t.IterateIncoming(false); t.IterateOutgoing(true); t.IterateOutgoing(false); }
}

static class Program
{
    public static string Show(byte[] b, int o, int n) => n > 64 ? $"{n}B" : Encoding.UTF8.GetString(b, o, n);

    static async Task<string> Grant(HttpClient h, string key, string pid)
    {
        var req = new HttpRequestMessage(HttpMethod.Post, "http://127.0.0.1:8090/v1/grants");
        req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        req.Content = new StringContent("{\"player_id\":\"" + pid + "\",\"name\":\"" + pid + "\"}", Encoding.UTF8, "application/json");
        var r = await h.SendAsync(req); var body = await r.Content.ReadAsStringAsync();
        if (!r.IsSuccessStatusCode) throw new Exception(body);
        int i = body.IndexOf("\"grant\":\"") + 9; return body.Substring(i, body.IndexOf('"', i) - i);
    }

    static async Task Pump(params IDriver[] ds) { var until = DateTime.UtcNow.AddMilliseconds(1500); while (DateTime.UtcNow < until) { foreach (var d in ds) d.Pump(); await Task.Delay(5); } }

    static async Task<bool> Scenario(Func<string, string, IDriver> make)
    {
        var host = make("host", "H"); var c1 = make("c1", "C1"); var c2 = make("c2", "C2");
        var log = new StringBuilder();
        Action<string> L = s => log.Append(s).Append(' ');
        host.Log = L; c1.Log = L; c2.Log = L;
        host.Log = s => { L(s); if (s.StartsWith("Hrx")) { int conn = int.Parse(s.Substring(3, s.IndexOf(':') - 3)); host.SendToClient(conn, Encoding.UTF8.GetBytes("pong"), false); } };

        host.Listen(); await Pump(host);
        if (!host.Listening) { Console.WriteLine($"[{host.Name}] FAIL host not listening"); return false; }
        c1.Connect(); c2.Connect(); await Pump(host, c1, c2);
        Console.WriteLine($"[{host.Name}] connections on host: {host.Peers}/2  c1={c1.Connected} c2={c2.Connected}");
        c1.SendToServer(Encoding.UTF8.GetBytes("ping1"), true); c2.SendToServer(Encoding.UTF8.GetBytes("ping2"), true); await Pump(host, c1, c2);
        host.SendToClient(host.FirstPeer, new byte[4000], true); await Pump(host, c1, c2);
        host.Kick(host.LastPeer); await Pump(host, c1, c2);
        host.StopListening(); await Pump(host, c1, c2);
        var s = log.ToString();
        Console.WriteLine($"[{host.Name}] log: {s}");
        bool ok = s.Contains("H+") && s.Contains("C1+") && s.Contains("C2+") && s.Contains("ping1") && s.Contains("ping2") && s.Contains("C1rx:pong") && s.Contains("C2rx:pong")
                  && s.Contains("rx:4000B") && s.Contains("ServerRequest") && s.Contains("C1-") && s.Contains("C2-") && host.Peers == 0 && !c1.Connected && !c2.Connected;
        Console.WriteLine($"[{host.Name}] {(ok ? "LIVE OK" : "LIVE FAIL")}");
        return ok;
    }

    static async Task<int> Main()
    {
        Console.WriteLine("TRANSPORT ADAPTERS COMPILE OK: " + typeof(Aurix.Net.PurrNet.AurixNetTransport).Name + ", " + typeof(Aurix.Net.MirrorTransport.AurixNetMirrorTransport).Name + ", " + typeof(Aurix.Net.FishNetTransport.AurixNetFishNetTransport).Name);
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        if (string.IsNullOrEmpty(key)) { Console.WriteLine("NET_API_KEY unset: live test skipped"); return 0; }
        var h = new HttpClient();
        bool all = true;
        foreach (var kind in new[] { "purr", "mirror", "fish" })
        {
            var room = kind + "-" + DateTime.UtcNow.Ticks;
            Func<string, string, IDriver> make = (pid, tag) =>
            {
                Func<Task<string>> g = () => Grant(h, key, kind + "-" + pid);
                return kind == "purr" ? new PurrDriver(room, g, tag) : kind == "mirror" ? new MirrorDriver(room, g, tag) : (IDriver)new FishDriver(room, g, tag);
            };
            all &= await Scenario(make);
        }
        return all ? 0 : 1;
    }
}
