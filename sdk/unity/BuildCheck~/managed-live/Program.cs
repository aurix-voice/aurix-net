using System.Collections.Generic;
using System;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Text;
using System.Threading.Tasks;
using Aurix.Net;
using Aurix.Net.Managed;

class P
{
    static async Task<string> Grant(HttpClient h, string key, string pid)
    {
        var req = new HttpRequestMessage(HttpMethod.Post, "http://127.0.0.1:8090/v1/grants");
        req.Headers.Authorization = new AuthenticationHeaderValue("Bearer", key);
        req.Content = new StringContent("{\"player_id\":\"" + pid + "\",\"name\":\"" + pid + "\"}", Encoding.UTF8, "application/json");
        var r = await h.SendAsync(req); var body = await r.Content.ReadAsStringAsync();
        if (!r.IsSuccessStatusCode) throw new Exception(body);
        int i = body.IndexOf("\"grant\":\"") + 9; return body.Substring(i, body.IndexOf('"', i) - i);
    }
    static async Task<int> Main()
    {
        var key = Environment.GetEnvironmentVariable("NET_API_KEY");
        var h = new HttpClient();
        var room = "cs-" + DateTime.UtcNow.Ticks;
        var a = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-a"));
        var b = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-b"));
        int rel = 0, unrel = 0, big = 0, bigLen = 0; string ctl = "";
        b.OnEvent += f => { if (f.Code == 1) rel++; else if (f.Code == 2) unrel++; else if (f.Code == 3) { big++; bigLen = f.Payload.Count; } };
        b.OnControl += j => { if (j.Contains("player_joined")) ctl += "J"; };
        int aa = await a.JoinOrCreateAsync(room, "relay", 8);
        int ab = await b.JoinAsync(room);
        Console.WriteLine($"joined actors a={aa} b={ab} mode={a.RoomMode}");
        a.SendEvent(1, Encoding.UTF8.GetBytes("hello"), Target.Others, reliable: true);
        for (int i = 0; i < 10; i++) a.SendEvent(2, new byte[] { (byte)i }, Target.Others, reliable: false);
        a.SendEvent(3, new byte[5000], Target.Others, reliable: true);
        a.Flush();
        var until = DateTime.UtcNow.AddSeconds(2);
        while (DateTime.UtcNow < until) { a.Poll(); b.Poll(); await Task.Delay(5); }
        Console.WriteLine($"relay: reliable={rel}/1 unreliable={unrel}/10 fragmented={big}/1 ({bigLen} bytes) control={ctl}");
        await a.LeaveAsync(); await b.LeaveAsync(); a.Dispose(); b.Dispose();

        var s = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-s"));
        int deltas = 0; s.OnData += (ch, tick, p) => { if ((p[0] & 0xf0) == 0x10) deltas++; };
        await s.JoinOrCreateAsync("cs-sync-" + room, "sync", 8, "{\"tick_hz\":60}");
        until = DateTime.UtcNow.AddSeconds(2); int sent = 0;
        while (DateTime.UtcNow < until) { s.SendData(Channel.Unreliable, new byte[] { 0x10, 0, 0, 0, 0, 1, 3, 127, 0, 0 }); sent++; s.Flush(); s.Poll(); await Task.Delay(16); }
        Console.WriteLine($"sync: inputs sent={sent} deltas received={deltas} (expect ~120 in 2 s)");
        await s.LeaveAsync(); s.Dispose();
        // owner-authoritative sync room with a custom schema: A moves its avatar, B decodes deltas
        var idl = "component Transform {\\n  pos: vec3 @range(-100, 100, 0.01)\\n  rot: quat\\n}\\ncomponent Tag {\\n  name: string(16)\\n  hp: u8\\n}\\n";
        var schema = Schema.Parse(idl.Replace("\\n", "\n"));
        var wa = new SyncWorld(schema); var wb = new SyncWorld(schema);
        var oa = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-oa"));
        var ob = await ManagedClient.ConnectAsync("http://127.0.0.1:8090", await Grant(h, key, "cs-ob"));
        oa.OnData += (ch, t, p) => { var ack = wa.Receive(p); if (ack != null) oa.SendData(Channel.Unreliable, ack); };
        ob.OnData += (ch, t, p) => { var ack = wb.Receive(p); if (ack != null) ob.SendData(Channel.Unreliable, ack); };
        int spawns = 0, changes = 0; wb.OnSpawn += e => spawns++; wb.OnChange += e => changes++;
        int actA = await oa.JoinOrCreateAsync("cs-owner-" + room, "sync", 8, "{\"schema\":\"" + idl + "\",\"tick_hz\":60}");
        var hash = Aurix.Net.Managed.ManagedClientTestHooks.Str(oa.JoinedJson, "schema_hash");
        await ob.JoinAsync("cs-owner-" + room, hash);
        var tr = schema.ByName("Transform"); var tag = schema.ByName("Tag");
        until = DateTime.UtcNow.AddSeconds(2); float x = 0;
        while (DateTime.UtcNow < until)
        {
            x = Math.Min(5f, x + 0.1f);
            var pos = ComponentValue.Zero(tr); pos.Fields[0] = new Value { X = x, Y = 1.5f, Z = -2 }; pos.Fields[1] = new Value { X = 0, Y = 0.7071f, Z = 0, W = 0.7071f };
            var tg = ComponentValue.Zero(tag); tg.Fields[0] = new Value { S = "alice" }; tg.Fields[1] = new Value { U = 77 };
            var body = wa.EncodeWrites(new List<(ComponentDef, ComponentValue)> { (tr, pos), (tag, tg) });
            oa.SendData(Channel.Unreliable, SyncWorld.EncodeInput(wa.ServerTick + 2, new[] { body }));
            oa.Flush(); ob.Flush(); oa.Poll(); ob.Poll(); await Task.Delay(16);
        }
        NetEntity seen = null; foreach (var e in wb.Entities.Values) if (e.Owner == actA) seen = e;
        var sp = seen?.Get(tr)?.Fields[0]; var sq = seen?.Get(tr)?.Fields[1]; var st = seen?.Get(tag);
        Console.WriteLine($"schema sync: entities on B={wb.Entities.Count} spawns={spawns} changes={changes} gaps={wb.Gaps} A.pos=({sp?.X:F2},{sp?.Y:F2},{sp?.Z:F2}) rot.y={sq?.Y:F3} tag={st?.Fields[0].S}/{st?.Fields[1].U}");
        bool schemaOk = seen != null && wb.Entities.Count == 2 && Math.Abs((sp?.X ?? 0) - 5f) < 0.02 && Math.Abs((sp?.Z ?? 0) + 2f) < 0.02 && Math.Abs((sq?.Y ?? 0) - 0.7071f) < 0.01 && st?.Fields[0].S == "alice" && st?.Fields[1].U == 77;
        await oa.LeaveAsync(); await ob.LeaveAsync(); oa.Dispose(); ob.Dispose();
        bool ok = schemaOk && rel == 1 && unrel == 10 && big == 1 && bigLen == 5000 && deltas > 60;
        Console.WriteLine(ok ? "MANAGED OK" : "MANAGED FAIL");
        return ok ? 0 : 1;
    }
}
