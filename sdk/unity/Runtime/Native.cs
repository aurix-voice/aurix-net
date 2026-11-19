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

        public const int EventNone = 0,
            EventControl = 1,
            EventData = 2,
            EventDataReady = 3,
            EventClosed = 4,
            EventUndelivered = 5;

        [StructLayout(LayoutKind.Sequential)]
        public struct Undelivered
        {
            public byte code;
            public int target;
            public UIntPtr nActors;
            public IntPtr data;
            public UIntPtr len;
            public int group;
            public int cache;
        }

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_version();

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_last_error();

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_issue_grant(
            string http,
            string apiKey,
            string playerId,
            string name
        );

        [DllImport(Lib)]
        public static extern void aurix_net_string_free(IntPtr pointer);

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_connect(string http, string grant, int tunnel);

        [DllImport(Lib)]
        public static extern int aurix_net_send_control(IntPtr handle, string json);

        [DllImport(Lib)]
        public static extern int aurix_net_join(IntPtr handle, string json);

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_joined_json(IntPtr handle);

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_player_id(IntPtr handle);

        [DllImport(Lib)]
        public static extern IntPtr aurix_net_player_name(IntPtr handle);

        [DllImport(Lib)]
        public static extern int aurix_net_send_data(
            IntPtr handle,
            byte channel,
            byte[] data,
            UIntPtr len
        );

        [DllImport(Lib)]
        public static extern int aurix_net_send_event(
            IntPtr handle,
            byte code,
            int target,
            ushort[] actors,
            UIntPtr nActors,
            byte[] data,
            UIntPtr len,
            int reliable
        );

        [DllImport(Lib)]
        public static extern int aurix_net_send_event_on(
            IntPtr handle,
            byte channel,
            byte code,
            int target,
            ushort[] actors,
            UIntPtr nActors,
            byte[] data,
            UIntPtr len,
            int group,
            int cache
        );

        [DllImport(Lib)]
        public static extern void aurix_net_flush(IntPtr handle);

        [DllImport(Lib)]
        public static extern int aurix_net_poll(IntPtr handle, out Event ev);

        [DllImport(Lib)]
        public static extern void aurix_net_event_free(ref Event ev);

        [DllImport(Lib)]
        public static extern UIntPtr aurix_net_undelivered_count(ref Event ev);

        [DllImport(Lib)]
        public static extern int aurix_net_undelivered_get(
            ref Event ev,
            UIntPtr index,
            out Undelivered sent
        );

        [DllImport(Lib)]
        public static extern UIntPtr aurix_net_undelivered_actors(
            ref Event ev,
            UIntPtr index,
            [Out] ushort[] actors,
            UIntPtr cap
        );

        [DllImport(Lib)]
        public static extern int aurix_net_leave(IntPtr handle);

        [DllImport(Lib)]
        public static extern void aurix_net_destroy(IntPtr handle);

        public static string Utf8(IntPtr pointer) =>
            pointer == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(pointer);

        public static string LastError() => Utf8(aurix_net_last_error()) ?? "unknown error";
    }
}
