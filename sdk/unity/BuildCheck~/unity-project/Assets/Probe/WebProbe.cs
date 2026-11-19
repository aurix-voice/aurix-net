using System;
using System.Text;
using Aurix.Net;
using Aurix.Net.Managed;
using UnityEngine;

// What only a browser can show of a Web build: the client connects through the plug-in, joins a
// room, gets its own reliable event back and measures the round trip. The page is opened as
// index.html?url=<node>&grant=<grant>; the result is one AURIX line on the page and in the console.
// For a hidden tab add &watch=<seconds>: after the result the probe sends a reliable event that
// often and says what it has seen, and &background=1 turns Run In Background on.
public class WebProbe : MonoBehaviour
{
    string _line = "AURIX web waits: open the page as index.html?url=<node>&grant=<grant>";
    ManagedClient _client;
    bool _joined,
        _judged;
    float _deadline;
    int _actor,
        _watch,
        _sent,
        _echoes,
        _updates;

    async void Start()
    {
        string url = Query("url"),
            grant = Query("grant");
        if (url == null || grant == null)
        {
            return;
        }

        Application.runInBackground = Query("background") == "1";
        int.TryParse(Query("watch"), out _watch);
        try
        {
            Say("AURIX web runs against " + url);
            _client = await ManagedClient.ConnectAsync(url, grant);
            _client.OnClosed += reason =>
            {
                _joined = false;
                Say("AURIX web closed: " + reason);
            };
            _client.OnEvent += frame =>
            {
                if (
                    frame.Code == 7
                    && frame.Reliable
                    && Encoding.UTF8.GetString(frame.Payload.Array, frame.Payload.Offset, frame.Payload.Count) == "probe"
                )
                {
                    _echoes++;
                }
            };
            _actor = await _client.JoinOrCreateAsync("web-probe-" + Guid.NewGuid().ToString("N").Substring(0, 8));
            Send();
            _deadline = Time.realtimeSinceStartup + 10f;
            _joined = true;
        }
        catch (Exception error)
        {
            Say("AURIX web FAIL " + error.GetType().Name + ": " + error.Message);
        }
    }

    void Update()
    {
        _client?.Poll();
        if (!_joined)
        {
            return;
        }

        _updates++;
        float now = Time.realtimeSinceStartup;
        if (_judged)
        {
            if (_watch > 0 && now >= _deadline)
            {
                Say(
                    "AURIX web alive at " + (int)now + " s: updates " + _updates + ", reliable echoes " + _echoes
                        + " of " + _sent + ", rtt " + _client.RttMs + " ms"
                );
                Send();
                _deadline = now + _watch;
            }
            return;
        }

        bool passed = _echoes > 0 && _client.RttMs > 0f;
        if (passed || now > _deadline)
        {
            Say(
                "AURIX web " + (passed ? "PASS" : "FAIL") + " editor " + Application.unityVersion
                    + ", actor " + _actor + ", reliable echo " + (_echoes > 0) + ", rtt " + _client.RttMs + " ms"
            );
            _judged = true;
            _deadline = now + _watch;
        }
    }

    void Send()
    {
        _client.SendEvent(7, Encoding.UTF8.GetBytes("probe"), Target.All, reliable: true);
        _sent++;
    }

    static string Query(string key)
    {
        int at = Application.absoluteURL.IndexOf('?');
        foreach (string pair in at < 0 ? new string[0] : Application.absoluteURL.Substring(at + 1).Split('&'))
        {
            if (pair.StartsWith(key + "="))
            {
                return Uri.UnescapeDataString(pair.Substring(key.Length + 1));
            }
        }
        return null;
    }

    void Say(string line)
    {
        _line = line;
        Debug.Log(line);
    }

    void OnGUI() => GUI.Label(new Rect(10, 10, Screen.width - 20, 60), _line);
}
