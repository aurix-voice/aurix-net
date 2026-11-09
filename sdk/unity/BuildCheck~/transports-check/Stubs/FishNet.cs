// Minimal mirror of FishNet.Transporting (Assets/FishNet/Runtime/Transporting, MIT) for compile checks only.
using System;
namespace FishNet.Managing { public class NetworkManager : UnityEngine.MonoBehaviour { } }
namespace FishNet.Transporting
{
    public enum Channel : byte { Reliable = 0, Unreliable = 1 }
    public enum LocalConnectionState : int { Stopped = 1, Stopping = 2, Starting = 4, Started = 8 }
    public enum RemoteConnectionState : byte { Stopped = 0, Started = 2 }
    public enum IPAddressType { IPv4, IPv6 }
    public struct ServerReceivedDataArgs { public ArraySegment<byte> Data; public Channel Channel; public int ConnectionId; public int TransportIndex; public ServerReceivedDataArgs(ArraySegment<byte> d, Channel c, int id, int t) { Data = d; Channel = c; ConnectionId = id; TransportIndex = t; } }
    public struct ClientReceivedDataArgs { public ArraySegment<byte> Data; public Channel Channel; public int TransportIndex; public ClientReceivedDataArgs(ArraySegment<byte> d, Channel c, int t) { Data = d; Channel = c; TransportIndex = t; } }
    public struct RemoteConnectionStateArgs { public int TransportIndex; public RemoteConnectionState ConnectionState; public int ConnectionId; public RemoteConnectionStateArgs(RemoteConnectionState s, int id, int t) { ConnectionState = s; ConnectionId = id; TransportIndex = t; } }
    public struct ServerConnectionStateArgs { public int TransportIndex; public LocalConnectionState ConnectionState; public ServerConnectionStateArgs(LocalConnectionState s, int t) { ConnectionState = s; TransportIndex = t; } }
    public struct ClientConnectionStateArgs { public LocalConnectionState ConnectionState; public int TransportIndex; public ClientConnectionStateArgs(LocalConnectionState s, int t) { ConnectionState = s; TransportIndex = t; } }
    public abstract class Transport : UnityEngine.MonoBehaviour
    {
        public int Index { get; private set; }
        public virtual void Initialize(FishNet.Managing.NetworkManager nm, int transportIndex) { Index = transportIndex; }
        public abstract string GetConnectionAddress(int connectionId);
        public abstract event Action<ClientConnectionStateArgs> OnClientConnectionState;
        public abstract event Action<ServerConnectionStateArgs> OnServerConnectionState;
        public abstract event Action<RemoteConnectionStateArgs> OnRemoteConnectionState;
        public abstract void HandleClientConnectionState(ClientConnectionStateArgs a);
        public abstract void HandleServerConnectionState(ServerConnectionStateArgs a);
        public abstract void HandleRemoteConnectionState(RemoteConnectionStateArgs a);
        public abstract LocalConnectionState GetConnectionState(bool server);
        public abstract RemoteConnectionState GetConnectionState(int connectionId);
        public abstract void SendToServer(byte channelId, ArraySegment<byte> segment);
        public abstract void SendToClient(byte channelId, ArraySegment<byte> segment, int connectionId);
        public abstract event Action<ClientReceivedDataArgs> OnClientReceivedData;
        public abstract void HandleClientReceivedDataArgs(ClientReceivedDataArgs a);
        public abstract event Action<ServerReceivedDataArgs> OnServerReceivedData;
        public abstract void HandleServerReceivedDataArgs(ServerReceivedDataArgs a);
        public abstract void IterateIncoming(bool asServer);
        public abstract void IterateOutgoing(bool asServer);
        public virtual int GetMaximumClients() => 0;
        public virtual void SetMaximumClients(int value) { }
        public virtual void SetClientAddress(string address) { }
        public virtual string GetClientAddress() => string.Empty;
        public abstract bool StartConnection(bool server);
        public abstract bool StopConnection(bool server);
        public abstract bool StopConnection(int connectionId, bool immediately);
        public abstract void Shutdown();
        public abstract int GetMTU(byte channel);
    }
}
