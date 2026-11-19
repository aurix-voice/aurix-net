using System;
using System.Collections.Generic;
using System.Net.WebSockets;
using System.Threading;
using System.Threading.Tasks;

namespace Aurix.Net.Managed
{
    /// <summary>What a socket tells the client that owns it.</summary>
    internal interface ISocketOwner
    {
        /// <summary>One whole message; the socket reads the next one when the task is done.</summary>
        Task Message(byte[] data, bool text);

        /// <summary>Every <see cref="SocketTiming.TickMs"/> while the socket is open.</summary>
        void Tick();

        /// <summary>The socket is gone and the client did not close it.</summary>
        void Closed(string reason);
    }

    /// <summary>The WebSocket of a client and the time it runs on: threads where the platform has them, the browser's own socket and timers in a Web build.</summary>
    internal interface ISocket : IDisposable
    {
        bool IsOpen { get; }
        Task OpenAsync(string url, int timeoutMs, ISocketOwner owner);
        Task SendAsync(byte[] data, bool text);
        Task Delay(int ms);

        /// <summary>Takes room for events that will wait for the game's Poll; <see cref="Release"/> gives it back.</summary>
        Task Admit(int events);
        void Release(int events);
    }

    internal static class SocketTiming
    {
        public const int TickMs = 10;
    }

    /// <summary>System.Net.WebSockets with a reader and a timer off the game thread. A reader that finds no room for an event stops reading until the game polls.</summary>
    internal sealed class SystemSocket : ISocket
    {
        readonly ClientWebSocket _ws = new ClientWebSocket();
        readonly CancellationTokenSource _cts = new CancellationTokenSource();
        readonly SemaphoreSlim _sendLock = new SemaphoreSlim(1, 1);
        readonly SemaphoreSlim _slots;
        ISocketOwner _owner;

        public SystemSocket(int backlog)
        {
            _slots = new SemaphoreSlim(backlog, backlog);
        }

        public bool IsOpen => _ws.State == WebSocketState.Open;

        public async Task OpenAsync(string url, int timeoutMs, ISocketOwner owner)
        {
            _owner = owner;
            using (var timeout = new CancellationTokenSource(timeoutMs))
            {
                await _ws.ConnectAsync(new Uri(url), timeout.Token);
            }

            _ = ReadLoop();
            _ = TimerLoop();
        }

        public async Task SendAsync(byte[] data, bool text)
        {
            await _sendLock.WaitAsync().ConfigureAwait(false);
            try
            {
                if (_ws.State == WebSocketState.Open)
                {
                    await _ws.SendAsync(
                            new ArraySegment<byte>(data),
                            text ? WebSocketMessageType.Text : WebSocketMessageType.Binary,
                            true,
                            _cts.Token
                        )
                        .ConfigureAwait(false);
                }
            }
            finally
            {
                _sendLock.Release();
            }
        }

        public Task Delay(int ms) => Task.Delay(ms);

        public async Task Admit(int events)
        {
            for (int i = 0; i < events; i++)
            {
                await _slots.WaitAsync(_cts.Token).ConfigureAwait(false);
            }
        }

        public void Release(int events) => _slots.Release(events);

        async Task TimerLoop()
        {
            try
            {
                while (_ws.State == WebSocketState.Open)
                {
                    await Task.Delay(SocketTiming.TickMs, _cts.Token).ConfigureAwait(false);
                    _owner.Tick();
                }
            }
            catch (OperationCanceledException) { }
        }

        async Task ReadLoop()
        {
            var buf = new byte[64 * 1024];
            var acc = new List<byte>();
            try
            {
                while (_ws.State == WebSocketState.Open)
                {
                    var result = await _ws.ReceiveAsync(new ArraySegment<byte>(buf), _cts.Token)
                        .ConfigureAwait(false);
                    if (result.MessageType == WebSocketMessageType.Close)
                    {
                        break;
                    }

                    for (int i = 0; i < result.Count; i++)
                    {
                        acc.Add(buf[i]);
                    }

                    if (!result.EndOfMessage)
                    {
                        continue;
                    }

                    var msg = acc.ToArray();
                    acc.Clear();
                    await _owner
                        .Message(msg, result.MessageType == WebSocketMessageType.Text)
                        .ConfigureAwait(false);
                }
                // the node's close frame says why: a code, and often a reason
                var code = _ws.CloseStatus;
                _owner.Closed(
                    code == null || code == WebSocketCloseStatus.Empty
                        ? ManagedClient.ControlClosed
                        : (
                            ManagedClient.ControlClosed
                            + ": "
                            + (int)code
                            + " "
                            + _ws.CloseStatusDescription
                        ).TrimEnd()
                );
            }
            catch (Exception error)
            {
                if (!_cts.IsCancellationRequested)
                {
                    _owner.Closed(error.Message);
                }
            }
        }

        public void Dispose()
        {
            try
            {
                _cts.Cancel();
                _ws.Dispose();
            }
            catch { }
        }
    }
}
