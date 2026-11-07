// Relay chat sample: a relay room, text as reliable events (code 1), player join/leave from control messages.
using System.Text;
using Aurix.Net;
using Aurix.Net.Unity;
using UnityEngine;

public sealed class ChatSample : MonoBehaviour
{
    public NetRunner Runner; public string Room = "chat-1";
    string _input = ""; readonly StringBuilder _log = new StringBuilder();

    async void Start()
    {
        Runner.OnEvent += f => { if (f.Code == 1) _log.AppendLine($"[{f.From}] {Encoding.UTF8.GetString(f.Payload.Array, f.Payload.Offset, f.Payload.Count)}"); };
        Runner.OnControl += j => { if (j.Contains("player_joined") || j.Contains("player_left")) _log.AppendLine(j); };
        await Runner.ConnectWithDevGrantAsync();
        await Runner.JoinOrCreateAsync(Room, RoomMode.Relay, 16);
        _log.AppendLine("joined as actor " + Runner.Actor);
    }

    void OnGUI()
    {
        GUILayout.BeginArea(new Rect(10, 10, 500, 400));
        GUILayout.TextArea(_log.ToString(), GUILayout.Height(340));
        GUILayout.BeginHorizontal();
        _input = GUILayout.TextField(_input);
        if (GUILayout.Button("Send", GUILayout.Width(60)) && Runner.IsJoined && _input.Length > 0)
        {
            Runner.SendEvent(1, Encoding.UTF8.GetBytes(_input), Target.All, reliable: true); _input = "";
        }
        GUILayout.EndHorizontal(); GUILayout.EndArea();
    }
}
