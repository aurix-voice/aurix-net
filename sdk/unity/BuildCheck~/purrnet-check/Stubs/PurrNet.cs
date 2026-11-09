// Minimal mirror of PurrNet.Transports (Assets/PurrNet/Runtime/Transports/Interface, MIT) for compile checks only.
using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
namespace PurrNet.Transports
{
    public delegate void OnConnectionState(ConnectionState state, bool asServer);
    public delegate void OnDataReceived(Connection conn, ByteData data, bool asServer);
    public delegate void OnDataSent(Connection conn, ByteData data, bool asServer);
    public delegate void OnConnected(Connection conn, bool asServer);
    public delegate void OnDisconnected(Connection conn, DisconnectReason reason, bool asServer);
    public enum ConnectionState { Connecting, Connected, Disconnected, Disconnecting }
    public enum DisconnectReason { Timeout, ClientRequest, ServerRequest, ConnectionFailed }
    public enum Channel : byte { ReliableUnordered, UnreliableSequenced, ReliableOrdered, Unreliable }
    public readonly struct ByteData
    {
        public readonly byte[] data; public readonly int length; public readonly int offset;
        public ByteData(byte[] data, int offset, int length) { this.data = data; this.offset = offset; this.length = length; }
    }
    public struct Connection : IEquatable<Connection>
    {
        public int connectionId { get; private set; }
        public bool isValid { get; private set; }
        public Connection(int connectionId) { this.connectionId = connectionId; isValid = true; }
        public bool Equals(Connection o) => connectionId == o.connectionId && isValid == o.isValid;
        public override bool Equals(object o) => o is Connection c && Equals(c);
        public override int GetHashCode() => connectionId;
    }
    public interface IConnectable { ConnectionState clientState { get; } void Connect(string ip, ushort port); void Disconnect(); }
    public interface IListener { ConnectionState listenerState { get; } void Listen(ushort port); void StopListening(); }
    public interface ITransport : IListener, IConnectable
    {
        event OnConnected onConnected; event OnDisconnected onDisconnected; event OnDataReceived onDataReceived; event OnDataSent onDataSent; event OnConnectionState onConnectionState;
        IReadOnlyList<Connection> connections { get; }
        int GetMTU(Connection target, Channel channel, bool asServer);
        void RaiseDataReceived(Connection conn, ByteData data, bool asServer);
        void RaiseDataSent(Connection conn, ByteData data, bool asServer);
        void SendToClient(Connection target, ByteData data, Channel method = Channel.ReliableOrdered);
        void SendToServer(ByteData data, Channel method = Channel.ReliableOrdered);
        void CloseConnection(Connection conn);
        void ReceiveMessages(float delta);
        void SendMessages(float delta);
    }
    public abstract class GenericTransport : UnityEngine.MonoBehaviour
    {
        public abstract bool isSupported { get; }
        public abstract ITransport transport { get; }
        protected abstract void StartClientInternal();
        protected abstract void StartServerInternal();
    }
}
