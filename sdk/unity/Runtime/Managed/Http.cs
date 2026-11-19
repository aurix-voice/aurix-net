using System.Diagnostics;
using System.Net.Http;
using System.Threading.Tasks;
#if UNITY_WEBGL && !UNITY_EDITOR
using UnityEngine.Networking;
#else
using System;
using System.Net.Http.Headers;
using System.Text;
using System.Threading;
#endif

namespace Aurix.Net.Managed
{
    internal struct HttpReply
    {
        public bool Ok;
        public string Body;

        /// <summary>From the request to its answer.</summary>
        public int Ms;
    }

    /// <summary>One HTTP request: UnityWebRequest in a Web build, where HttpClient is not implemented, and HttpClient everywhere else. Throws HttpRequestException when no answer came.</summary>
    internal static class Http
    {
#if UNITY_WEBGL && !UNITY_EDITOR
        public static Task<HttpReply> SendAsync(
            string method,
            string url,
            int timeoutMs,
            string bearer = null,
            string json = null
        )
        {
            var done = new TaskCompletionSource<HttpReply>();
            var clock = Stopwatch.StartNew();
            var request = new UnityWebRequest(url, method)
            {
                downloadHandler = new DownloadHandlerBuffer(),
                timeout = (timeoutMs + 999) / 1000,
            };
            if (bearer != null)
            {
                request.SetRequestHeader("Authorization", "Bearer " + bearer);
            }

            if (json != null)
            {
                request.uploadHandler = new UploadHandlerRaw(
                    System.Text.Encoding.UTF8.GetBytes(json)
                )
                {
                    contentType = "application/json",
                };
            }

            request.SendWebRequest().completed += _ =>
            {
                using (request)
                {
                    if (
                        request.result == UnityWebRequest.Result.Success
                        || request.result == UnityWebRequest.Result.ProtocolError
                    )
                    {
                        done.SetResult(
                            new HttpReply
                            {
                                Ok = request.result == UnityWebRequest.Result.Success,
                                Body = request.downloadHandler.text,
                                Ms = (int)clock.ElapsedMilliseconds,
                            }
                        );
                    }
                    else
                    {
                        done.SetException(new HttpRequestException(request.error));
                    }
                }
            };
            return done.Task;
        }
#else
        static readonly HttpClient Client = new HttpClient { Timeout = Timeout.InfiniteTimeSpan };

        public static async Task<HttpReply> SendAsync(
            string method,
            string url,
            int timeoutMs,
            string bearer = null,
            string json = null
        )
        {
            var clock = Stopwatch.StartNew();
            using (var request = new HttpRequestMessage(new HttpMethod(method), url))
            using (var timeout = new CancellationTokenSource(timeoutMs))
            {
                if (bearer != null)
                {
                    request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", bearer);
                }

                if (json != null)
                {
                    request.Content = new StringContent(json, Encoding.UTF8, "application/json");
                }

                using (
                    var response = await Client
                        .SendAsync(request, timeout.Token)
                        .ConfigureAwait(false)
                )
                {
                    return new HttpReply
                    {
                        Ok = response.IsSuccessStatusCode,
                        Body = await response.Content.ReadAsStringAsync().ConfigureAwait(false),
                        Ms = (int)clock.ElapsedMilliseconds,
                    };
                }
            }
        }
#endif
    }
}
