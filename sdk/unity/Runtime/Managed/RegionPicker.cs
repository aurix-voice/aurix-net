using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Net.Http;
using System.Threading.Tasks;
using Newtonsoft.Json.Linq;

namespace Aurix.Net
{
    public struct RegionCandidate
    {
        public string Region,
            DisplayName,
            Endpoint;
        public int RttMs;
    }

    public struct RegionPick
    {
        public string Region,
            Endpoint;
        public int RttMs;
        public List<RegionCandidate> Candidates;
    }

    /// <summary>Region list and best-region ping: reads /v1/regions from the catalog (or any node), probes every live
    /// endpoint's /healthz and returns the fastest; pass Endpoint to ManagedClient.ConnectAsync.</summary>
    public static class RegionPicker
    {
        public static async Task<RegionPick> BestRegionAsync(
            string catalogHttp,
            int probes = 3,
            int timeoutMs = 3000
        )
        {
            using var http = new HttpClient { Timeout = TimeSpan.FromMilliseconds(timeoutMs) };
            var baseUrl = catalogHttp.TrimEnd('/');
            var json = await http.GetStringAsync(baseUrl + "/v1/regions").ConfigureAwait(false);
            var regions = JArray.Parse(json);
            probes = Math.Max(1, Math.Min(10, probes));
            var tasks = new List<Task<RegionCandidate>>();
            foreach (var region in regions)
            {
                var id = (string)region["id"] ?? "";
                var name = (string)region["display_name"] ?? "";
                foreach (
                    var ep in region["endpoints"]?.Values<string>() ?? Enumerable.Empty<string>()
                )
                {
                    var endpoint = ep.TrimEnd('/');
                    tasks.Add(
                        Task.Run(async () =>
                        {
                            int best = -1;
                            for (int i = 0; i < probes; i++)
                            {
                                var sw = Stopwatch.StartNew();
                                try
                                {
                                    using var _ = await http.GetAsync(endpoint + "/healthz")
                                        .ConfigureAwait(false);
                                    var ms = (int)sw.ElapsedMilliseconds;
                                    best = best < 0 ? ms : Math.Min(best, ms);
                                }
                                catch
                                { /* unreachable */
                                }
                            }
                            return new RegionCandidate
                            {
                                Region = id,
                                DisplayName = name,
                                Endpoint = endpoint,
                                RttMs = best,
                            };
                        })
                    );
                }
            }
            var candidates = (await Task.WhenAll(tasks).ConfigureAwait(false)).ToList();
            var ok = candidates
                .Where(candidate => candidate.RttMs >= 0)
                .OrderBy(candidate => candidate.RttMs)
                .ToList();
            if (ok.Count == 0)
            {
                throw new InvalidOperationException("no reachable region");
            }

            return new RegionPick
            {
                Region = ok[0].Region,
                Endpoint = ok[0].Endpoint,
                RttMs = ok[0].RttMs,
                Candidates = candidates,
            };
        }
    }
}
