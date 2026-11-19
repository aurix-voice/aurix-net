#if UNITY_WEBGL && !UNITY_EDITOR
using System;
using System.Runtime.InteropServices;

namespace Aurix.Net.Managed
{
    /// <summary>The functions of Plugins/WebGL/AurixNet.jslib.</summary>
    internal static class Browser
    {
        [DllImport("__Internal", EntryPoint = "AurixNetOpen")]
        public static extern int Open(
            string url,
            Action<int> onOpen,
            Action<int, IntPtr, int, int> onMessage,
            Action<int, int> onClose
        );

        [DllImport("__Internal", EntryPoint = "AurixNetSend")]
        public static extern void Send(int socket, byte[] data, int length, int text);

        [DllImport("__Internal", EntryPoint = "AurixNetClose")]
        public static extern void Close(int socket);

        [DllImport("__Internal", EntryPoint = "AurixNetEvery")]
        public static extern int Every(int ms, Action<int> onTimer);

        [DllImport("__Internal", EntryPoint = "AurixNetAfter")]
        public static extern int After(int ms, Action<int> onTimer);

        [DllImport("__Internal", EntryPoint = "AurixNetCancel")]
        public static extern void Cancel(int timer);
    }
}
#endif
