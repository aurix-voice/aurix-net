// The page a Web build runs in, without a browser: one thread, sockets and timers that call the
// client back between the page's tasks. It stands where the JavaScript plug-in and the engine's
// UnityWebRequest stand in a build, so the Web side of the client runs as it is under plain dotnet.
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace Aurix.Net.Managed
{
    internal static class Browser
    {
        public static int Open(string url, Action<int> onOpen, Action<int, IntPtr, int, int> onMessage, Action<int, int, IntPtr, int> onClose) =>
            Page.Open(url, onOpen, onMessage, onClose);

        public static void Send(int socket, byte[] data, int length, int text) => Page.Send(socket, data, length, text != 0);

        public static void Close(int socket) => Page.Close(socket);

        public static int Every(int ms, Action<int> onTimer)
        {
            int id = 0;
            return id = Page.Every(ms, () => onTimer(id));
        }

        public static int After(int ms, Action<int> onTimer)
        {
            int id = 0;
            return id = Page.After(ms, () => onTimer(id));
        }

        public static void Cancel(int timer) => Page.Cancel(timer);
    }
}

/// <summary>What answers a socket of the page. Its methods run as tasks of the page.</summary>
public abstract class Node
{
    internal int Socket;

    public virtual void Opened() { }

    public virtual void Text(string json) { }

    public virtual void Binary(byte[] data) { }

    public void Say(string json) => Page.Deliver(Socket, Encoding.UTF8.GetBytes(json), true);

    public void Say(byte[] data) => Page.Deliver(Socket, data, false);

    public void Hang(int code = 1000, string reason = "") => Page.Drop(Socket, code, reason);
}

public static class Page
{
    sealed class Timer
    {
        public double Due;
        public int Every;
        public Action Call;
    }

    sealed class Socket
    {
        public Node Node;
        public bool Open;
        public Action<int, IntPtr, int, int> OnMessage;
        public Action<int, int, IntPtr, int> OnClose;
    }

    static readonly Stopwatch Clock = Stopwatch.StartNew();

    /// <summary>A hidden tab: the browser runs timers no more often than this, in ms. What a socket brings is not held.</summary>
    public static int Throttle;
    static double _wake;
    static readonly Dictionary<int, Timer> Timers = new Dictionary<int, Timer>();
    static readonly Dictionary<int, Socket> Sockets = new Dictionary<int, Socket>();
    static readonly ConcurrentQueue<Action> Tasks = new ConcurrentQueue<Action>();
    static int _next;

    /// <summary>The nodes of the network by the URL of their socket; one with no node behind it never answers.</summary>
    public static readonly Dictionary<string, Func<Node>> Nodes = new Dictionary<string, Func<Node>>();

    /// <summary>The answers of the network to HTTP requests by URL; null is a host that is not there.</summary>
    public static readonly Dictionary<string, Func<(long Status, string Body, int AfterMs)>> Http =
        new Dictionary<string, Func<(long, string, int)>>();

    public static int Thread { get; private set; }
    public static int LiveTimers => Timers.Count;
    public static int LiveSockets => Sockets.Count;

    /// <summary>As the engine's own: what a task awaits on the page goes on in a task of the page.</summary>
    sealed class Context : SynchronizationContext
    {
        public override void Post(SendOrPostCallback callback, object state) => Tasks.Enqueue(() => callback(state));
    }

    /// <summary>The engine and the browser serve one thread, and a Web build has no other.</summary>
    public static void OnThread()
    {
        if (Environment.CurrentManagedThreadId != Thread)
        {
            throw new InvalidOperationException("called off the thread of the page");
        }
    }

    public static void Reset()
    {
        SynchronizationContext.SetSynchronizationContext(new Context());
        Throttle = 0;
        _wake = 0;
        Timers.Clear();
        Sockets.Clear();
        Tasks.Clear();
        Nodes.Clear();
        Http.Clear();
        UnityEngine.Networking.UnityWebRequest.Sent.Clear();
        _next = 1;
        Thread = Environment.CurrentManagedThreadId;
    }

    /// <summary>Runs the page until <paramref name="until"/> holds; false when it did not within <paramref name="ms"/>.</summary>
    public static bool Run(Func<bool> until, int ms = 5000)
    {
        double end = Clock.Elapsed.TotalMilliseconds + ms;
        while (!until())
        {
            if (Clock.Elapsed.TotalMilliseconds > end)
            {
                return false;
            }

            for (int tasks = Tasks.Count; tasks > 0 && Tasks.TryDequeue(out var task); tasks--)
            {
                task();
            }

            double now = Clock.Elapsed.TotalMilliseconds;
            if (now < _wake)
            {
                System.Threading.Thread.Sleep(1);
                continue;
            }

            _wake = now + Throttle;
            foreach (int id in new List<int>(Timers.Keys))
            {
                if (Timers.TryGetValue(id, out var timer) && now >= timer.Due)
                {
                    if (timer.Every > 0)
                    {
                        timer.Due = Math.Max(timer.Due + timer.Every, now);
                    }
                    else
                    {
                        Timers.Remove(id);
                    }

                    timer.Call();
                }
            }
            System.Threading.Thread.Sleep(1);
        }
        return true;
    }

    public static void Run(int ms) => Run(() => false, ms);

    public static int Every(int ms, Action call)
    {
        OnThread();
        Timers[_next] = new Timer { Due = Clock.Elapsed.TotalMilliseconds + ms, Every = ms, Call = call };
        return _next++;
    }

    public static int After(int ms, Action call)
    {
        OnThread();
        Timers[_next] = new Timer { Due = Clock.Elapsed.TotalMilliseconds + ms, Call = call };
        return _next++;
    }

    public static void Cancel(int timer)
    {
        OnThread();
        Timers.Remove(timer);
    }

    internal static int Open(string url, Action<int> onOpen, Action<int, IntPtr, int, int> onMessage, Action<int, int, IntPtr, int> onClose)
    {
        OnThread();
        int id = _next++;
        var socket = new Socket { OnMessage = onMessage, OnClose = onClose };
        Sockets[id] = socket;
        if (!Nodes.TryGetValue(url, out var node))
        {
            Tasks.Enqueue(() => Closed(id, 1006, ""));
        }
        else if (node != null)
        {
            Tasks.Enqueue(() =>
            {
                if (Sockets.ContainsKey(id))
                {
                    socket.Open = true;
                    socket.Node = node();
                    socket.Node.Socket = id;
                    onOpen(id);
                    socket.Node.Opened();
                }
            });
        }
        return id;
    }

    internal static void Send(int id, byte[] data, int length, bool text)
    {
        OnThread();
        if (Sockets.TryGetValue(id, out var socket) && socket.Open)
        {
            var sent = new byte[length];
            Array.Copy(data, sent, length);
            Tasks.Enqueue(() =>
            {
                if (text)
                {
                    socket.Node.Text(Encoding.UTF8.GetString(sent));
                }
                else
                {
                    socket.Node.Binary(sent);
                }
            });
        }
    }

    internal static void Close(int id)
    {
        OnThread();
        Sockets.Remove(id);
    }

    internal static void Deliver(int id, byte[] data, bool text) =>
        Tasks.Enqueue(() =>
        {
            if (Sockets.TryGetValue(id, out var socket))
            {
                var buffer = Marshal.AllocHGlobal(data.Length + 1);
                Marshal.Copy(data, 0, buffer, data.Length);
                try
                {
                    socket.OnMessage(id, buffer, data.Length, text ? 1 : 0);
                }
                finally
                {
                    Marshal.FreeHGlobal(buffer);
                }
            }
        });

    internal static void Drop(int id, int code, string reason) =>
        Tasks.Enqueue(() => Closed(id, code, reason));

    static void Closed(int id, int code, string reason)
    {
        if (Sockets.Remove(id, out var socket))
        {
            var said = Encoding.UTF8.GetBytes(reason);
            var buffer = Marshal.AllocHGlobal(said.Length + 1);
            Marshal.Copy(said, 0, buffer, said.Length);
            try
            {
                socket.OnClose(id, code, buffer, said.Length);
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }
    }
}

namespace AOT
{
    public sealed class MonoPInvokeCallbackAttribute : Attribute
    {
        public MonoPInvokeCallbackAttribute(Type type) { }
    }
}

namespace UnityEngine
{
    public class AsyncOperation
    {
        Action<AsyncOperation> _completed;
        bool _done;

        public event Action<AsyncOperation> completed
        {
            add
            {
                if (_done)
                {
                    value(this);
                }
                else
                {
                    _completed += value;
                }
            }
            remove => _completed -= value;
        }

        internal void Complete()
        {
            _done = true;
            _completed?.Invoke(this);
        }
    }
}

namespace UnityEngine.Networking
{
    public class DownloadHandlerBuffer
    {
        public string text;
    }

    public class UploadHandlerRaw
    {
        public readonly byte[] data;
        public string contentType;

        public UploadHandlerRaw(byte[] data)
        {
            this.data = data;
        }
    }

    public class UnityWebRequestAsyncOperation : AsyncOperation { }

    public class UnityWebRequest : IDisposable
    {
        public enum Result
        {
            InProgress,
            Success,
            ConnectionError,
            ProtocolError,
            DataProcessingError,
        }

        public readonly string url,
            method;
        public readonly Dictionary<string, string> headers = new Dictionary<string, string>();
        public DownloadHandlerBuffer downloadHandler;
        public UploadHandlerRaw uploadHandler;
        public int timeout;
        public bool disposed;

        public static readonly List<UnityWebRequest> Sent = new List<UnityWebRequest>();

        public UnityWebRequest(string url, string method)
        {
            Page.OnThread();
            this.url = url;
            this.method = method;
        }

        public Result result { get; private set; }
        public string error { get; private set; }

        public void SetRequestHeader(string name, string value) => headers[name] = value;

        public UnityWebRequestAsyncOperation SendWebRequest()
        {
            Page.OnThread();
            Sent.Add(this);
            var operation = new UnityWebRequestAsyncOperation();
            Page.Http.TryGetValue(url, out var answer);
            if (answer == null)
            {
                Page.After(1, () =>
                {
                    result = Result.ConnectionError;
                    error = "Cannot connect to destination host";
                    operation.Complete();
                });
                return operation;
            }

            var (status, body, afterMs) = answer();
            Page.After(afterMs, () =>
            {
                result = status >= 400 ? Result.ProtocolError : Result.Success;
                error = status >= 400 ? "HTTP/1.1 " + status : null;
                downloadHandler.text = body;
                operation.Complete();
            });
            return operation;
        }

        public void Dispose() => disposed = true;
    }
}
