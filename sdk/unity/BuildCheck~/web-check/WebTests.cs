// The client as it runs in a Web build: the browser's socket and timers, no threads. The page and
// the node are in Page.cs; what only a browser can show is left to the build check of the editor.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net;
using Aurix.Net.Managed;
using NUnit.Framework;
using UnityEngine.Networking;

[TestFixture]
public class WebTests
{
    const string Url = "http://node";
    const string SocketUrl = "ws://node/ws";

    // A node with one room: the control channel in text, the data path in the frames of a link.
    sealed class RoomNode : Node
    {
        static readonly byte[] Ticket = { 7, 7, 7 };
        public readonly Link Link = new Link();
        public readonly List<string> Control = new List<string>();
        public readonly List<byte[]> Got = new List<byte[]>();
        public int LoseFromClient,
            LoseToClient,
            PongAfterMs,
            Flood;
        public bool Deaf;
        bool _ticket;

        public override void Opened() => Page.Every(10, () => { Link.Poll(); Flush(); });

        public override void Text(string json)
        {
            Control.Add(json);
            switch (Json.Str(json, "t"))
            {
                case "auth":
                    Say("{\"t\":\"authed\",\"player_id\":\"ann\",\"name\":\"Ann\"}");
                    break;
                case "ping":
                    string pong = "{\"t\":\"pong\",\"ts\":" + (long)Json.Num(json, "ts") + ",\"server_ms\":0}";
                    Page.After(PongAfterMs, () => Say(pong));
                    break;
                case "join":
                    Say(
                        "{\"t\":\"joined\",\"room\":\"r1\",\"name\":\"lobby\",\"actor\":1,\"mode\":\"relay\",\"data\":{\"ticket\":\""
                            + Convert.ToBase64String(Ticket)
                            + "\"}}"
                    );
                    break;
                case "go":
                    for (int i = 0; i < Flood; i++)
                    {
                        Say("{\"t\":\"notice\",\"text\":\"" + i + "\",\"level\":\"info\"}");
                    }
                    break;
            }
        }

        public override void Binary(byte[] data)
        {
            if (!_ticket)
            {
                _ticket = data.SequenceEqual(Ticket);
                Send(new byte[0]);
                return;
            }

            if (Deaf)
            {
                return;
            }

            if (LoseFromClient > 0)
            {
                LoseFromClient--;
                return;
            }

            Got.AddRange(Link.Receive(data).Select(received => received.Payload));
            Flush();
        }

        public void Send(byte[] payload)
        {
            Link.Send(Channel.Reliable, payload);
            Flush();
        }

        void Flush()
        {
            while (Link.TryDequeueOutgoing(out var packet))
            {
                if (LoseToClient > 0)
                {
                    LoseToClient--;
                    continue;
                }

                Say(packet);
            }
        }
    }

    RoomNode _node;

    [SetUp]
    public void Open()
    {
        Page.Reset();
        _node = new RoomNode();
        Page.Nodes[SocketUrl] = () => _node;
    }

    static T Wait<T>(Task<T> task, int ms = 5000)
    {
        Page.Run(() => task.IsCompleted, ms);
        Assert.That(task.IsCompleted, "the page ran " + ms + " ms and the task did not end");
        return task.GetAwaiter().GetResult();
    }

    static ManagedClient Joined()
    {
        var client = Wait(ManagedClient.ConnectAsync(Url, "grant"));
        Assert.That(Wait(client.JoinAsync("lobby")), Is.EqualTo(1));
        return client;
    }

    [Test]
    public void ConnectsThroughTheSocketOfTheBrowser()
    {
        using var client = Wait(ManagedClient.ConnectAsync(Url, "grant"));
        Assert.That(client.PlayerId, Is.EqualTo("ann"));
        Assert.That(Json.Str(_node.Control[0], "grant"), Is.EqualTo("grant"));
    }

    [Test]
    public void ConnectFailsWhenTheSocketCloses()
    {
        var connect = ManagedClient.ConnectAsync("http://nobody", "grant");
        var error = Assert.Throws<NetException>(() => Wait(connect));
        Assert.That(error.Message, Does.Contain("1006"));
        Assert.That((Page.LiveSockets, Page.LiveTimers), Is.EqualTo((0, 0)));
    }

    [Test]
    public void ConnectTimesOutByTheTimerOfTheBrowser()
    {
        Page.Nodes["ws://silent/ws"] = null;
        var connect = ManagedClient.ConnectAsync("http://silent", "grant", 100);
        var error = Assert.Throws<NetException>(() => Wait(connect));
        Assert.That(error.Message, Is.EqualTo("connect timeout"));
        Assert.That((Page.LiveSockets, Page.LiveTimers), Is.EqualTo((0, 0)));
    }

    [Test]
    public void JoinOpensTheDataPath()
    {
        using var client = Joined();
        var got = new List<byte[]>();
        client.OnData += (channel, tick, payload) => got.Add(payload);
        client.SendData(Channel.Reliable, new byte[] { 0x31, 1, 2 });
        _node.Send(new byte[] { 0x32, 3, 4 });
        Page.Run(() => client.Poll() < 0 || (_node.Got.Count == 1 && got.Count == 1));
        Assert.That(_node.Got, Is.EqualTo(new[] { new byte[] { 0x31, 1, 2 } }));
        Assert.That(got, Is.EqualTo(new[] { new byte[] { 0x32, 3, 4 } }));
    }

    [Test]
    public void ReliableMessageIsSentAgainUntilTheNodeHasIt()
    {
        using var client = Joined();
        _node.LoseFromClient = 2;
        client.SendData(Channel.Reliable, new byte[] { 0x31, 9 });
        Page.Run(() => client.Poll() < 0 || _node.Got.Count > 0);
        Page.Run(300);
        Assert.That(_node.Got, Is.EqualTo(new[] { new byte[] { 0x31, 9 } }));
    }

    [Test]
    public void ReliableMessageOfTheNodeArrivesOnceAfterALoss()
    {
        using var client = Joined();
        var got = new List<byte[]>();
        client.OnData += (channel, tick, payload) => got.Add(payload);
        _node.LoseToClient = 1;
        _node.Send(new byte[] { 0x32, 5 });
        Page.Run(() => client.Poll() < 0 || got.Count > 0);
        Page.Run(() => client.Poll() < 0, 300);
        Assert.That(got, Is.EqualTo(new[] { new byte[] { 0x32, 5 } }));
    }

    [Test]
    public void ResendsGoOnWhileTheGameDrawsNoFrame()
    {
        using var client = Joined();
        _node.LoseFromClient = 2;
        client.SendData(Channel.Reliable, new byte[] { 0x31, 9 });
        Page.Run(() => _node.Got.Count > 0);
        Assert.That(_node.Got, Is.EqualTo(new[] { new byte[] { 0x31, 9 } }));
    }

    [Test]
    public void TimerOfAHiddenTabResendsBeforeTheLinkIsGivenUp()
    {
        using var client = Joined();
        string closed = null;
        client.OnClosed += reason => closed = reason;
        Page.Run(50);
        _node.LoseFromClient = 1;
        client.SendData(Channel.Reliable, new byte[] { 0x31, 9 });
        Page.Throttle = (int)Link.IdleTimeout + 2000;
        Page.Run(() => _node.Got.Count > 0, 2 * (int)Link.IdleTimeout + 5000);
        client.Poll();
        Assert.That(_node.Got, Is.EqualTo(new[] { new byte[] { 0x31, 9 } }));
        Assert.That(closed, Is.Null);
    }

    [Test]
    public void ProbesMeasureTheRoundTrip()
    {
        _node.PongAfterMs = 40;
        using var client = Wait(ManagedClient.ConnectAsync(Url, "grant"));
        var pongs = 0;
        client.OnControl += json => pongs += Json.Str(json, "t") == "pong" ? 1 : 0;
        Page.Run(() => client.Poll() < 0 || client.RttMs > 0f, 3000);
        Assert.That(client.RttMs, Is.InRange(35f, 200f));
        Assert.That(pongs, Is.Zero, "the answer to the client's own probe is not the game's business");
    }

    [Test]
    public void LinkTheNodeStoppedAnsweringIsDead()
    {
        using var client = Joined();
        string closed = null;
        client.OnClosed += reason => closed = reason;
        _node.Deaf = true;
        client.SendData(Channel.Reliable, new byte[] { 0x31, 9 });
        var clock = System.Diagnostics.Stopwatch.StartNew();
        Page.Run(() => client.Poll() < 0 || closed != null, (int)Link.IdleTimeout + 5000);
        Assert.That(closed, Is.EqualTo("link dead"));
        Assert.That(clock.ElapsedMilliseconds, Is.GreaterThanOrEqualTo((long)Link.IdleTimeout - 1000));
        Assert.Throws<NetException>(() => client.SendData(Channel.Reliable, new byte[] { 0x31, 9 }));
    }

    [Test]
    public void DataPathTheNodeGaveUpIsDead()
    {
        using var client = Joined();
        string closed = null;
        client.OnClosed += reason => closed = reason;
        _node.Say("{\"t\":\"error\",\"code\":\"transport\",\"message\":\"data path closed\"}");
        Page.Run(() => client.Poll() < 0 || closed != null);
        Assert.That(closed, Is.EqualTo("link dead"));
    }

    [Test]
    public void ClosedSocketClosesTheClient()
    {
        using var client = Joined();
        string closed = null;
        client.OnClosed += reason => closed = reason;
        _node.Hang();
        Page.Run(() => client.Poll() < 0 || closed != null);
        Assert.That(closed, Is.EqualTo("control channel closed"));
        Assert.That(Page.LiveTimers, Is.EqualTo(1), "what is left is the timer of the node");
    }

    [Test]
    public void SocketClosesWhenTheGameDoesNotPoll()
    {
        using var client = Wait(ManagedClient.ConnectAsync(Url, "grant"));
        var notices = new List<string>();
        string closed = null;
        client.OnNotice += (text, level) => notices.Add(text);
        client.OnClosed += reason => closed = reason;
        _node.Flood = ManagedClient.EventBacklog + 100;
        Wait(client.SendControlAsync("{\"t\":\"go\"}").ContinueWith(_ => true));
        Assert.That(Page.Run(() => Page.LiveSockets == 0), "the socket stayed open");
        Assert.That(ManagedClientTestHooks.Waiting(client), Is.EqualTo(ManagedClient.EventBacklog + 1));

        Page.Run(() => client.Poll() == 0);
        Assert.That(notices, Is.EqualTo(Enumerable.Range(0, ManagedClient.EventBacklog).Select(i => i.ToString())));
        Assert.That(closed, Is.EqualTo("event backlog full"));
    }

    [Test]
    public void DisposedClientLeavesNothingOnThePage()
    {
        var client = Joined();
        client.Dispose();
        Assert.That((Page.LiveSockets, Page.LiveTimers), Is.EqualTo((0, 1)), "what is left is the timer of the node");
    }

    [Test]
    public void EverythingRunsOnTheThreadOfThePage()
    {
        var threads = new List<int>();
        bool closed = false;
        void Here() => threads.Add(Environment.CurrentManagedThreadId);
        async Task Play()
        {
            using var client = await ManagedClient.ConnectAsync(Url, "grant");
            Here();
            client.OnControl += _ => Here();
            client.OnData += (channel, tick, payload) => Here();
            client.OnClosed += _ =>
            {
                Here();
                closed = true;
            };
            await client.JoinAsync("lobby");
            Here();
            _node.Send(new byte[] { 0x32, 5 });
            _node.Hang();
            Page.Run(() => client.Poll() < 0 || closed);
        }
        Wait(Play().ContinueWith(task => task.IsFaulted ? throw task.Exception : true));
        Assert.That(threads, Has.Count.GreaterThanOrEqualTo(5));
        Assert.That(threads, Is.All.EqualTo(Page.Thread));
    }

    [Test]
    public void RegionIsPickedOverTheRequestsOfTheEngine()
    {
        Page.Http["http://catalog/v1/regions"] = () =>
            (
                200,
                "[{\"id\":\"eu\",\"display_name\":\"Europe\",\"endpoints\":[\"http://eu/\"]},"
                    + "{\"id\":\"us\",\"display_name\":\"America\",\"endpoints\":[\"http://us\",\"http://gone\"]}]",
                1
            );
        Page.Http["http://eu/healthz"] = () => (200, "ok", 60);
        Page.Http["http://us/healthz"] = () => (503, "draining", 20);
        var pick = Wait(RegionPicker.BestRegionAsync("http://catalog/", 2));
        Assert.That((pick.Region, pick.Endpoint), Is.EqualTo(("us", "http://us")));
        Assert.That(pick.RttMs, Is.InRange(15, 55));
        Assert.That(pick.Candidates.Select(candidate => candidate.RttMs < 0), Is.EqualTo(new[] { false, false, true }));
        Assert.That(UnityWebRequest.Sent, Has.Count.EqualTo(7));
        Assert.That(UnityWebRequest.Sent.All(request => request.disposed && request.timeout == 3));
    }

    [Test]
    public void RegionListThatIsRefusedIsAnError()
    {
        Page.Http["http://catalog/v1/regions"] = () => (500, "broken", 1);
        var pick = RegionPicker.BestRegionAsync("http://catalog");
        Assert.That(Assert.Throws<System.Net.Http.HttpRequestException>(() => Wait(pick)).Message, Does.Contain("broken"));
        Assert.Throws<System.Net.Http.HttpRequestException>(() => Wait(RegionPicker.BestRegionAsync("http://gone")));
    }

    [Test]
    public void RequestCarriesTheKeyAndTheBody()
    {
        Page.Http["http://node/v1/grants"] = () => (403, "{\"error\":\"forbidden\"}", 1);
        var reply = Wait(Http.SendAsync("POST", "http://node/v1/grants", 1500, "key", "{\"player_id\":\"ann\"}"));
        Assert.That((reply.Ok, reply.Body), Is.EqualTo((false, "{\"error\":\"forbidden\"}")));
        var request = UnityWebRequest.Sent.Single();
        Assert.That((request.method, request.timeout), Is.EqualTo(("POST", 2)));
        Assert.That(request.headers["Authorization"], Is.EqualTo("Bearer key"));
        Assert.That(request.uploadHandler.contentType, Is.EqualTo("application/json"));
        Assert.That(Encoding.UTF8.GetString(request.uploadHandler.data), Is.EqualTo("{\"player_id\":\"ann\"}"));
    }
}
