// P/Invoke bindings for libaurix_net (crates/net-ffi/include/aurix_net.h).
using System;
using System.Runtime.InteropServices;

namespace Aurix.Net
{
    internal static class Native
    {
#if UNITY_IOS && !UNITY_EDITOR
        const string Lib = "__Internal";
#else
        const string Lib = "aurix_net";
#endif
        [StructLayout(LayoutKind.Sequential)]
        public struct Event
        {
            public int kind;
            public byte channel;
            public ushort tick;
            public IntPtr data;
            public UIntPtr dataLen;
            public IntPtr json;
        }

        public const int EventNone = 0, EventControl = 1, EventData = 2, EventDataReady = 3, EventClosed = 4;

        [DllImport(Lib)] public static extern IntPtr aurix_net_version();
        [DllImport(Lib)] public static extern IntPtr aurix_net_last_error();
        [DllImport(Lib)] public static extern IntPtr aurix_net_issue_grant(string http, string apiKey, string playerId, string name);
        [DllImport(Lib)] public static extern void aurix_net_string_free(IntPtr s);
        [DllImport(Lib)] public static extern IntPtr aurix_net_connect(string http, string grant, int tunnel);
        [DllImport(Lib)] public static extern int aurix_net_send_control(IntPtr h, string json);
        [DllImport(Lib)] public static extern int aurix_net_join(IntPtr h, string json);
        [DllImport(Lib)] public static extern IntPtr aurix_net_joined_json(IntPtr h);
        [DllImport(Lib)] public static extern int aurix_net_send_data(IntPtr h, byte channel, byte[] data, UIntPtr len);
        [DllImport(Lib)] public static extern int aurix_net_send_event(IntPtr h, byte code, int target, ushort[] actors, UIntPtr nActors, byte[] data, UIntPtr len, int reliable);
        [DllImport(Lib)] public static extern void aurix_net_flush(IntPtr h);
        [DllImport(Lib)] public static extern int aurix_net_poll(IntPtr h, out Event ev);
        [DllImport(Lib)] public static extern void aurix_net_event_free(ref Event ev);
        [DllImport(Lib)] public static extern int aurix_net_leave(IntPtr h);
        [DllImport(Lib)] public static extern void aurix_net_destroy(IntPtr h);

        public static string Utf8(IntPtr p) => p == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(p);
        public static string LastError() => Utf8(aurix_net_last_error()) ?? "unknown error";
    }
}
