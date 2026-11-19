// Compile check + live test of the PurrNet / Mirror / FishNet transports over a local node
// (the live part skips without NET_API_KEY). The same scenario runs through each adapter:
// a connect cancelled half way, host + two clients, ping/pong both ways (reliable up, unreliable
// down, the channel checked on arrival), a 4000-byte reliable frame, a kick, host stop. Two
// adapters also run host mode: server and client on one transport, plus a remote client.
using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Nito.AsyncEx;

interface IDriver
{
    string Name { get; }
    void Listen(); void StopListening(); bool Listening { get; } int Peers { get; }
    void Connect(); void Disconnect(); bool Connected { get; }
    /// <summary>Received frames are logged with /r or /u when the framework is told the channel.</summary>
    bool ReportsChannel { get; }
    /// <summary>The framework starts a host as server + client on the same transport.</summary>
    bool HostMode { get; }
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
        t.roomName = room; t.GrantProvider = grant; t.httpUrl = Program.Http;
        t.onConnected += (c, s) => Log?.Invoke(s ? $"H+{c.connectionId}" : $"{tag}+");
        t.onDisconnected += (c, r, s) => Log?.Invoke(s ? $"H-{c.connectionId}:{r}" : $"{tag}-:{r}");
        t.onDataReceived += (c, d, s) => Log?.Invoke((s ? $"Hrx{c.connectionId}:" : $"{tag}rx:") + Program.Show(d.data, d.offset, d.length));
    }
    public void Listen() => t.Listen(0); public void StopListening() => t.StopListening();
    public bool Listening => t.listenerState == PurrNet.Transports.ConnectionState.Connected; public int Peers => t.connections.Count;
    public void Connect() => t.Connect(null, 0); public void Disconnect() => t.Disconnect(); public bool Connected => t.clientState == PurrNet.Transports.ConnectionState.Connected;
    public bool ReportsChannel => false; public bool HostMode => true;
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
        t.roomName = room; t.GrantProvider = grant; t.httpUrl = Program.Http;
        // Like the framework's NetworkServer: both connect callbacks lead to one handler, and an id that is
        // already connected (or 0, the local player) is kicked.
        Action<int> connected = c => { if (c == 0 || peers.Contains(c)) { Log?.Invoke($"DUP{c}"); t.ServerDisconnect(c); return; } peers.Add(c); Log?.Invoke($"H+{c}"); };
        t.OnServerConnected += connected;
        t.OnServerConnectedWithAddress += (c, addr) => connected(c);
        t.OnServerDisconnected = c => { peers.Remove(c); Log?.Invoke($"H-{c}:ServerRequest"); };
        t.OnServerDataReceived = (c, d, ch) => Log?.Invoke($"Hrx{c}:" + Program.Show(d.Array, d.Offset, d.Count) + Program.Ch(ch == Mirror.Channels.Reliable));
        t.OnClientConnected = () => Log?.Invoke($"{tag}+");
        t.OnClientDisconnected = () => Log?.Invoke($"{tag}-:{last}");
        t.OnClientError = (e, m) => last = e.ToString();
        t.OnClientDataReceived = (d, ch) => Log?.Invoke($"{tag}rx:" + Program.Show(d.Array, d.Offset, d.Count) + Program.Ch(ch == Mirror.Channels.Reliable));
    }
    string last = "ClientRequest";
    public void Listen() => t.ServerStart(); public void StopListening() => t.ServerStop();
    public bool Listening => t.ServerActive(); public int Peers => peers.Count;
    // Through the URI, as the framework's discovery does: the room name has to survive the round trip.
    public void Connect() => t.ClientConnect(t.ServerUri()); public void Disconnect() => t.ClientDisconnect(); public bool Connected => t.ClientConnected();
    public bool ReportsChannel => true; public bool HostMode => false;
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
        t.roomName = room; t.GrantProvider = grant; t.httpUrl = Program.Http; t.Initialize(null, 0);
        t.OnRemoteConnectionState += a => { if (a.ConnectionState == FishNet.Transporting.RemoteConnectionState.Started) { peers.Add(a.ConnectionId); Log?.Invoke($"H+{a.ConnectionId}"); } else { peers.Remove(a.ConnectionId); Log?.Invoke($"H-{a.ConnectionId}:ServerRequest"); } };
        t.OnServerReceivedData += a => Log?.Invoke($"Hrx{a.ConnectionId}:" + Program.Show(a.Data.Array, a.Data.Offset, a.Data.Count) + Program.Ch(a.Channel == FishNet.Transporting.Channel.Reliable));
        t.OnClientConnectionState += a => { if (a.ConnectionState == FishNet.Transporting.LocalConnectionState.Started) Log?.Invoke($"{tag}+"); else if (a.ConnectionState == FishNet.Transporting.LocalConnectionState.Stopped) Log?.Invoke($"{tag}-:Stopped"); };
        t.OnClientReceivedData += a => Log?.Invoke($"{tag}rx:" + Program.Show(a.Data.Array, a.Data.Offset, a.Data.Count) + Program.Ch(a.Channel == FishNet.Transporting.Channel.Reliable));
    }
    public void Listen() => t.StartConnection(true); public void StopListening() => t.StopConnection(true);
    public bool Listening => t.GetConnectionState(true) == FishNet.Transporting.LocalConnectionState.Started; public int Peers => peers.Count;
    public void Connect() => t.StartConnection(false); public void Disconnect() => t.StopConnection(false); public bool Connected => t.GetConnectionState(false) == FishNet.Transporting.LocalConnectionState.Started;
    public bool ReportsChannel => true; public bool HostMode => true;
    public void SendToServer(byte[] d, bool r) => t.SendToServer(r ? (byte)0 : (byte)1, new ArraySegment<byte>(d));
    public void SendToClient(int c, byte[] d, bool r) => t.SendToClient(r ? (byte)0 : (byte)1, new ArraySegment<byte>(d), c);
    public void Kick(int c) => t.StopConnection(c, true);
    public int FirstPeer => peers[0]; public int LastPeer => peers[peers.Count - 1];
    public void Pump() { t.IterateIncoming(true); t.IterateIncoming(false); t.IterateOutgoing(true); t.IterateOutgoing(false); }
}

static class Program
{
    public static string Show(byte[] b, int o, int n) => n > 64 ? $"{n}B" : Encoding.UTF8.GetString(b, o, n);
    public static string Ch(bool reliable) => reliable ? "/r" : "/u";

    // NET_SLOW_HTTP puts a delaying proxy between the transports and the node (see sdk/cpp/README.md)
    public static readonly string Http = Environment.GetEnvironmentVariable("NET_SLOW_HTTP") ?? "http://127.0.0.1:8090";
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
        var host = make("host", "H"); var c1 = make("c1", "C1"); var c2 = make("c2", "C2"); var c3 = make("c3", "C3");
        var log = new StringBuilder();
        Action<string> L = s => log.Append(s).Append(' ');
        host.Log = L; c1.Log = L; c2.Log = L; c3.Log = L;
        host.Log = s => { L(s); if (s.StartsWith("Hrx")) { int conn = int.Parse(s.Substring(3, s.IndexOf(':') - 3)); host.SendToClient(conn, Encoding.UTF8.GetBytes("pong"), false); } };

        host.Listen(); await Pump(host);
        if (!host.Listening) { Console.WriteLine($"[{host.Name}] FAIL host not listening"); return false; }
        // A connect cancelled before it completes must not leave a player in the room.
        c3.Connect(); c3.Disconnect(); await Pump(host, c3);
        bool cancelled = host.Peers == 0 && !c3.Connected && !log.ToString().Contains("C3+");
        c1.Connect(); c2.Connect(); await Pump(host, c1, c2);
        Console.WriteLine($"[{host.Name}] connections on host: {host.Peers}/2  c1={c1.Connected} c2={c2.Connected}");
        if (host.Peers != 2) { Console.WriteLine($"[{host.Name}] log: {log}\n[{host.Name}] LIVE FAIL"); host.StopListening(); await Pump(host, c1, c2, c3); return false; }
        c1.SendToServer(Encoding.UTF8.GetBytes("ping1"), true); c2.SendToServer(Encoding.UTF8.GetBytes("ping2"), true); await Pump(host, c1, c2);
        host.SendToClient(host.FirstPeer, new byte[4000], true); await Pump(host, c1, c2);
        host.Kick(host.LastPeer); await Pump(host, c1, c2);
        host.StopListening(); await Pump(host, c1, c2);
        var s = log.ToString();
        Console.WriteLine($"[{host.Name}] log: {s}");
        bool ok = s.Contains("H+") && s.Contains("C1+") && s.Contains("C2+") && s.Contains("ping1") && s.Contains("ping2") && s.Contains("C1rx:pong") && s.Contains("C2rx:pong")
                  && s.Contains("rx:4000B") && s.Contains("ServerRequest") && s.Contains("C1-") && s.Contains("C2-") && host.Peers == 0 && !c1.Connected && !c2.Connected
                  && cancelled && !s.Contains("DUP");
        if (host.ReportsChannel) ok &= s.Contains("ping1/r") && s.Contains("ping2/r") && s.Contains("C1rx:pong/u") && s.Contains("C2rx:pong/u") && s.Contains("rx:4000B/r");
        Console.WriteLine($"[{host.Name}] {(ok ? "LIVE OK" : "LIVE FAIL")}");
        return ok;
    }

    // Host mode: the transport is asked to listen and to connect at once (L is its client half), a
    // remote client joins the same room, the local client leaves and comes back, the host stops.
    static async Task<bool> HostMode(Func<string, string, IDriver> make)
    {
        var host = make("host", "L"); var c1 = make("c1", "C1");
        var log = new StringBuilder();
        Action<string> L = s => log.Append(s).Append(' ');
        c1.Log = L;
        host.Log = s => { L(s); if (s.StartsWith("Hrx")) { int conn = int.Parse(s.Substring(3, s.IndexOf(':') - 3)); host.SendToClient(conn, Encoding.UTF8.GetBytes("pong"), false); } };

        host.Listen(); host.Connect(); await Pump(host);
        bool up = host.Listening && host.Connected && host.Peers == 1;
        int local = up ? host.FirstPeer : -1;
        c1.Connect(); await Pump(host, c1);
        up &= c1.Connected && host.Peers == 2;
        host.SendToServer(Encoding.UTF8.GetBytes("lping"), true); c1.SendToServer(Encoding.UTF8.GetBytes("ping1"), true); await Pump(host, c1);
        if (up) host.SendToClient(local, new byte[4000], true); await Pump(host, c1);
        host.Disconnect(); await Pump(host, c1);
        bool left = !host.Connected && host.Listening && host.Peers == 1 && c1.Connected;
        host.Connect(); await Pump(host, c1);
        bool back = host.Connected && host.Peers == 2;
        host.StopListening(); await Pump(host, c1);
        var s = log.ToString();
        Console.WriteLine($"[{host.Name}] host mode log: {s}");
        bool ok = up && left && back && s.Contains($"Hrx{local}:lping") && s.Contains("Lrx:pong") && s.Contains("Lrx:4000B") && s.Contains("ping1") && s.Contains("C1rx:pong")
                  && s.Contains($"H-{local}") && s.Contains("L-") && s.Contains("C1-") && host.Peers == 0 && !host.Connected && !c1.Connected;
        if (host.ReportsChannel) ok &= s.Contains("lping/r") && s.Contains("Lrx:pong/u") && s.Contains("Lrx:4000B/r");
        Console.WriteLine($"[{host.Name}] {(ok ? "HOST MODE OK" : "HOST MODE FAIL")}");
        return ok;
    }

    // The engine runs the SDK's continuations on its main thread; this context does the same here.
    static int Main() => AsyncContext.Run(Run);

    static async Task<int> Run()
    {
        Console.WriteLine("TRANSPORT ADAPTERS COMPILE OK: " + typeof(Aurix.Net.PurrNet.AurixNetTransport).Name + ", " + typeof(Aurix.Net.MirrorTransport.AurixNetMirrorTransport).Name + ", " + typeof(Aurix.Net.FishNetTransport.AurixNetFishNetTransport).Name);
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        if (string.IsNullOrEmpty(key)) { Console.WriteLine("NET_API_KEY unset: live test skipped"); return 0; }
        var h = new HttpClient();
        bool all = true;
        foreach (var kind in new[] { "purr", "mirror", "fish" })
        {
            var room = kind + " Room_" + DateTime.UtcNow.Ticks;
            Func<string, string, IDriver> make = (pid, tag) =>
            {
                Func<Task<string>> g = () => Grant(h, key, kind + "-" + pid);
                return kind == "purr" ? new PurrDriver(room, g, tag) : kind == "mirror" ? new MirrorDriver(room, g, tag) : (IDriver)new FishDriver(room, g, tag);
            };
            all &= await Scenario(make);
            if (make("probe", "P").HostMode) { room += "h"; all &= await HostMode(make); }
        }
        return all ? 0 : 1;
    }
}
