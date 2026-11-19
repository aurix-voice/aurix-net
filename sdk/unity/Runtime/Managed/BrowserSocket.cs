#if UNITY_WEBGL && !UNITY_EDITOR
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using AOT;

namespace Aurix.Net.Managed
{
    /// <summary>The browser's WebSocket and timers through the JavaScript plug-in: a Web build has no System.Net sockets and no threads. The timer runs on the page, so the link goes on while no frame is drawn. A browser cannot stop reading, so a socket whose events the game does not poll closes when they fill the backlog.</summary>
    internal sealed class BrowserSocket : ISocket
    {
        static readonly Dictionary<int, BrowserSocket> Sockets =
            new Dictionary<int, BrowserSocket>();
        static readonly Dictionary<int, Action> Timers = new Dictionary<int, Action>();

        readonly int _backlog;
        ISocketOwner _owner;
        TaskCompletionSource<bool> _opening;
        int _id;
        int _ticker;
        int _deadline;
        int _held;
        bool _open;

        public BrowserSocket(int backlog)
        {
            _backlog = backlog;
        }

        public bool IsOpen => _open;

        public Task OpenAsync(string url, int timeoutMs, ISocketOwner owner)
        {
            _owner = owner;
            _opening = new TaskCompletionSource<bool>();
            _id = Browser.Open(url, OnOpen, OnMessage, OnClose);
            Sockets[_id] = this;
            _deadline = After(
                timeoutMs,
                () =>
                {
                    _deadline = 0;
                    Fail(new NetException("connect timeout"));
                }
            );
            return _opening.Task;
        }

        public Task SendAsync(byte[] data, bool text)
        {
            if (_open)
            {
                Browser.Send(_id, data, data.Length, text ? 1 : 0);
            }

            return Task.CompletedTask;
        }

        public Task Delay(int ms)
        {
            var done = new TaskCompletionSource<bool>();
            After(ms, () => done.TrySetResult(true));
            return done.Task;
        }

        public Task Admit(int events)
        {
            _held += events;
            return Task.CompletedTask;
        }

        public void Release(int events) => _held -= events;

        static int After(int ms, Action then)
        {
            int id = Browser.After(ms, OnTimer);
            Timers[id] = () =>
            {
                Timers.Remove(id);
                then();
            };
            return id;
        }

        void Fail(Exception error)
        {
            Drop();
            _opening.TrySetException(error);
        }

        void Drop()
        {
            _open = false;
            Sockets.Remove(_id);
            Browser.Close(_id);
            Cancel(ref _ticker);
            Cancel(ref _deadline);
        }

        static void Cancel(ref int timer)
        {
            if (timer != 0)
            {
                Browser.Cancel(timer);
                Timers.Remove(timer);
                timer = 0;
            }
        }

        public void Dispose() => Drop();

        [MonoPInvokeCallback(typeof(Action<int>))]
        static void OnOpen(int id)
        {
            if (Sockets.TryGetValue(id, out var socket))
            {
                Cancel(ref socket._deadline);
                socket._open = true;
                socket._ticker = Browser.Every(SocketTiming.TickMs, OnTimer);
                Timers[socket._ticker] = socket._owner.Tick;
                socket._opening.TrySetResult(true);
            }
        }

        [MonoPInvokeCallback(typeof(Action<int, IntPtr, int, int>))]
        static void OnMessage(int id, IntPtr data, int length, int text)
        {
            if (Sockets.TryGetValue(id, out var socket))
            {
                if (socket._held >= socket._backlog)
                {
                    socket.Drop();
                    socket._owner.Closed("event backlog full");
                    return;
                }

                var message = new byte[length];
                Marshal.Copy(data, message, 0, length);
                var taken = socket._owner.Message(message, text != 0);
                if (taken.IsFaulted)
                {
                    socket.Drop();
                    socket._owner.Closed(taken.Exception.InnerException.Message);
                }
            }
        }

        [MonoPInvokeCallback(typeof(Action<int, int>))]
        static void OnClose(int id, int code)
        {
            if (Sockets.TryGetValue(id, out var socket))
            {
                bool wasOpen = socket._open;
                socket.Drop();
                if (wasOpen)
                {
                    socket._owner.Closed("control channel closed");
                }
                else
                {
                    socket._opening.TrySetException(
                        new NetException("connect failed: socket closed with code " + code)
                    );
                }
            }
        }

        [MonoPInvokeCallback(typeof(Action<int>))]
        static void OnTimer(int id)
        {
            if (Timers.TryGetValue(id, out var timer))
            {
                timer();
            }
        }
    }
}
#endif
