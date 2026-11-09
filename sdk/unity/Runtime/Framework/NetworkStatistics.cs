// Aurix Net Unity SDK — connection statistics (ping, jitter, throughput, stall detection).
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>
    /// Per-manager network statistics, fed by <see cref="NetworkManager"/>: round-trip ping and jitter to the server
    /// (client side), per-player ping (server side), upload/download in bytes per second and messages per second,
    /// plus threshold events for high ping / jitter and a stalled connection.
    /// </summary>
    public sealed class NetworkStatistics
    {
        public float pingIntervalSeconds = 0.33f;
        public int highPingMs = 150, highJitterMs = 50;
        public float stallSeconds = 2f;

        /// <summary>Smoothed round trip to the server in ms (client side; 0 on a pure server).</summary>
        public int ping { get; private set; }
        public int lastPing { get; private set; }
        /// <summary>Smoothed |rtt − mean| in ms.</summary>
        public int jitter { get; private set; }
        public bool hasPingEstimate { get; private set; }
        /// <summary>Bytes per second over the last completed one-second window.</summary>
        public float upload { get; private set; }
        public float download { get; private set; }
        public int messagesOutPerSecond { get; private set; }
        public int messagesInPerSecond { get; private set; }
        public long totalBytesOut { get; private set; }
        public long totalBytesIn { get; private set; }
        public bool isHighPing { get; private set; }
        public bool isHighJitter { get; private set; }
        public bool isConnectionStalled { get; private set; }
        public float secondsSinceLastReceived => _connected ? _sinceReceived : 0f;

        public event Action<bool, int> onHighPingChanged;
        public event Action<bool, int> onHighJitterChanged;
        public event Action<bool, float> onConnectionStalledChanged;
        public event Action<int> onPingUpdated;

        readonly Dictionary<PlayerID, int> _playerPing = new Dictionary<PlayerID, int>();
        /// <summary>Server side: the last ping a client reported.</summary>
        public int PingOf(PlayerID p) => _playerPing.TryGetValue(p, out var v) ? v : -1;
        public IReadOnlyDictionary<PlayerID, int> playerPings => _playerPing;

        float _window, _sinceReceived, _sincePing; long _winOut, _winIn; int _winMsgOut, _winMsgIn; float _meanRtt; bool _connected;

        internal void Reset() { _playerPing.Clear(); ping = jitter = lastPing = 0; hasPingEstimate = false; _meanRtt = 0; _sinceReceived = 0; isConnectionStalled = false; _connected = false; }
        internal void SetConnected(bool c) { _connected = c; if (!c) { isConnectionStalled = false; _sinceReceived = 0; } }
        internal void CountOut(int bytes) { _winOut += bytes; _winMsgOut++; totalBytesOut += bytes; }
        internal void CountIn(int bytes) { _winIn += bytes; _winMsgIn++; totalBytesIn += bytes; _sinceReceived = 0; if (isConnectionStalled) { isConnectionStalled = false; onConnectionStalledChanged?.Invoke(false, 0f); } }
        internal void SetPlayerPing(PlayerID p, int ms) => _playerPing[p] = ms;
        internal void RemovePlayer(PlayerID p) => _playerPing.Remove(p);

        /// <summary>Returns true when a ping should be sent now.</summary>
        internal bool Update(float dt)
        {
            _window += dt; _sinceReceived += dt; _sincePing += dt;
            if (_window >= 1f)
            {
                upload = _winOut / _window; download = _winIn / _window; messagesOutPerSecond = (int)(_winMsgOut / _window); messagesInPerSecond = (int)(_winMsgIn / _window);
                _window = 0; _winOut = _winIn = 0; _winMsgOut = _winMsgIn = 0;
            }
            if (_connected && !isConnectionStalled && _sinceReceived > stallSeconds) { isConnectionStalled = true; onConnectionStalledChanged?.Invoke(true, _sinceReceived); }
            if (_sincePing >= pingIntervalSeconds) { _sincePing = 0; return true; }
            return false;
        }

        internal void RecordRtt(float rttMs)
        {
            lastPing = (int)Math.Round(rttMs);
            if (!hasPingEstimate) { _meanRtt = rttMs; ping = lastPing; jitter = 0; hasPingEstimate = true; }
            else
            {
                float dev = Math.Abs(rttMs - _meanRtt);
                _meanRtt += (rttMs - _meanRtt) * 0.2f; ping = (int)Math.Round(_meanRtt);
                jitter = (int)Math.Round(jitter + (dev - jitter) * 0.2f);
            }
            onPingUpdated?.Invoke(ping);
            bool hp = ping > highPingMs; if (hp != isHighPing) { isHighPing = hp; onHighPingChanged?.Invoke(hp, ping); }
            bool hj = jitter > highJitterMs; if (hj != isHighJitter) { isHighJitter = hj; onHighJitterChanged?.Invoke(hj, jitter); }
        }
    }
}
