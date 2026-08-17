var UPDATER_MANIFEST =
  "https://github.com/Gru110110110/deepseek-harness-desktop-launcher/releases/latest/download/latest.json";

export default {
  async fetch(request, env) {
    var url = new URL(request.url);
    if (url.pathname !== "/latest.json") {
      return env.ASSETS.fetch(request);
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("Method Not Allowed", {
        status: 405,
        headers: { Allow: "GET, HEAD" },
      });
    }

    try {
      var upstream = await fetch(UPDATER_MANIFEST, {
        method: request.method,
        redirect: "follow",
        cf: { cacheEverything: true, cacheTtl: 300 },
      });
      if (!upstream.ok) {
        return new Response("Updater manifest unavailable", {
          status: 502,
          headers: { "Cache-Control": "no-store" },
        });
      }

      var headers = new Headers(upstream.headers);
      headers.set("Access-Control-Allow-Origin", "*");
      headers.set("Cache-Control", "public, max-age=300, stale-if-error=86400");
      headers.set("Content-Type", "application/json; charset=utf-8");
      headers.set("X-Content-Type-Options", "nosniff");
      return new Response(upstream.body, {
        status: 200,
        headers: headers,
      });
    } catch (_error) {
      return new Response("Updater manifest unavailable", {
        status: 502,
        headers: { "Cache-Control": "no-store" },
      });
    }
  },
};
