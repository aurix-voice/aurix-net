using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net.Http;
using System.Threading.Tasks;

namespace Aurix.Net
{
    public struct RegionCandidate { public string Region, DisplayName, Endpoint; public int RttMs; }
    public struct RegionPick { public string Region, Endpoint; public int RttMs; public List<RegionCandidate> Candidates; }

    /// <summary>Photon GetRegions + best-region ping: reads /v1/regions from the catalog (or any node), probes every live
    /// endpoint's /healthz and returns the fastest; pass Endpoint to ManagedClient.ConnectAsync.</summary>
    /// <summary>Just enough JSON for /v1/regions (flat objects with string fields and string arrays).</summary>
    static class RegionJson
    {
        public static List<string> Objects(string json)
        {
            var list = new List<string>(); int depth = 0, start = -1; bool str = false;
            for (int i = 0; i < json.Length; i++)
            {
                char c = json[i];
                if (str) { if (c == '\\') i++; else if (c == '"') str = false; continue; }
                if (c == '"') str = true;
                else if (c == '{') { if (depth++ == 0) start = i; }
                else if (c == '}') { if (--depth == 0 && start >= 0) { list.Add(json.Substring(start, i - start + 1)); start = -1; } }
            }
            return list;
        }
        public static string Str(string obj, string key)
        {
            var m = System.Text.RegularExpressions.Regex.Match(obj, "\"" + key + "\"\\s*:\\s*\"((?:[^\"\\\\]|\\\\.)*)\"");
            return m.Success ? System.Text.RegularExpressions.Regex.Unescape(m.Groups[1].Value) : "";
        }
        public static List<string> StrArray(string obj, string key)
        {
            var list = new List<string>();
            var m = System.Text.RegularExpressions.Regex.Match(obj, "\"" + key + "\"\\s*:\\s*\\[([^\\]]*)\\]");
            if (!m.Success) return list;
            foreach (System.Text.RegularExpressions.Match s in System.Text.RegularExpressions.Regex.Matches(m.Groups[1].Value, "\"((?:[^\"\\\\]|\\\\.)*)\"")) list.Add(System.Text.RegularExpressions.Regex.Unescape(s.Groups[1].Value));
            return list;
        }
    }

    public static class RegionPicker
    {
        public static async Task<RegionPick> BestRegionAsync(string catalogHttp, int probes = 3, int timeoutMs = 3000)
        {
            using var http = new HttpClient { Timeout = TimeSpan.FromMilliseconds(timeoutMs) };
            var baseUrl = catalogHttp.TrimEnd('/');
            var json = await http.GetStringAsync(baseUrl + "/v1/regions").ConfigureAwait(false);
            var regions = RegionJson.Objects(json);
            probes = Math.Max(1, Math.Min(10, probes));
            var tasks = new List<Task<RegionCandidate>>();
            foreach (var r in regions)
            {
                var id = RegionJson.Str(r, "id"); var name = RegionJson.Str(r, "display_name");
                foreach (var ep in RegionJson.StrArray(r, "endpoints"))
                {
                    var endpoint = ep.TrimEnd('/');
                    tasks.Add(Task.Run(async () =>
                    {
                        int best = -1;
                        for (int i = 0; i < probes; i++)
                        {
                            var sw = Stopwatch.StartNew();
                            try { using var _ = await http.GetAsync(endpoint + "/healthz").ConfigureAwait(false); var ms = (int)sw.ElapsedMilliseconds; best = best < 0 ? ms : Math.Min(best, ms); }
                            catch { /* unreachable */ }
                        }
                        return new RegionCandidate { Region = id, DisplayName = name, Endpoint = endpoint, RttMs = best };
                    }));
                }
            }
            var candidates = (await Task.WhenAll(tasks).ConfigureAwait(false)).ToList();
            var ok = candidates.Where(c => c.RttMs >= 0).OrderBy(c => c.RttMs).ToList();
            if (ok.Count == 0) throw new InvalidOperationException("no reachable region");
            return new RegionPick { Region = ok[0].Region, Endpoint = ok[0].Endpoint, RttMs = ok[0].RttMs, Candidates = candidates };
        }
    }
}
