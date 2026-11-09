// Minimal mirror of Mirror's Transport base (Assets/Mirror/Core/Transport.cs, MIT) for compile checks only.
using System;
namespace Mirror
{
    public static class Channels { public const int Reliable = 0; public const int Unreliable = 1; }
    public enum TransportError : byte { DnsResolve, Refused, Timeout, Congestion, InvalidReceive, InvalidSend, ConnectionClosed, Unexpected }
    public abstract class Transport : UnityEngine.MonoBehaviour
    {
        public abstract bool Available();
        public Action OnClientConnected;
        public Action<ArraySegment<byte>, int> OnClientDataReceived;
        public Action<ArraySegment<byte>, int> OnClientDataSent;
        public Action<TransportError, string> OnClientError;
        public Action OnClientDisconnected;
        public Action<int> OnServerConnected;
        public Action<int, string> OnServerConnectedWithAddress;
        public Action<int, ArraySegment<byte>, int> OnServerDataReceived;
        public Action<int, ArraySegment<byte>, int> OnServerDataSent;
        public Action<int, TransportError, string> OnServerError;
        public Action<int> OnServerDisconnected;
        public abstract bool ClientConnected();
        public abstract void ClientConnect(string address);
        public abstract void ClientSend(ArraySegment<byte> segment, int channelId = Channels.Reliable);
        public abstract void ClientDisconnect();
        public abstract Uri ServerUri();
        public abstract bool ServerActive();
        public abstract void ServerStart();
        public abstract void ServerSend(int connectionId, ArraySegment<byte> segment, int channelId = Channels.Reliable);
        public abstract void ServerDisconnect(int connectionId);
        public abstract string ServerGetClientAddress(int connectionId);
        public abstract void ServerStop();
        public abstract int GetMaxPacketSize(int channelId = Channels.Reliable);
        public virtual int GetBatchThreshold(int channelId = Channels.Reliable) => GetMaxPacketSize(channelId);
        public virtual void ClientEarlyUpdate() { }
        public virtual void ServerEarlyUpdate() { }
        public virtual void ClientLateUpdate() { }
        public virtual void ServerLateUpdate() { }
        public abstract void Shutdown();
    }
}
