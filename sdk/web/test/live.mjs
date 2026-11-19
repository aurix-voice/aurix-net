// The live tests need a node and NET_API_KEY, a key of a project on it. Without the key they are
// skipped; with NET_LIVE_REQUIRED set a live test that cannot run fails instead.
export const key = process.env.NET_API_KEY;
export const required = !!process.env.NET_LIVE_REQUIRED;
if (required && !key) throw new Error("NET_LIVE_REQUIRED is set and NET_API_KEY is not");
