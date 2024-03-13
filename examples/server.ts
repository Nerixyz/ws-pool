Deno.serve({ port: 8080 }, (req) => {
  if (req.headers.get("upgrade") != "websocket") {
    return new Response(null, { status: 501 });
  }

  const { socket, response } = Deno.upgradeWebSocket(req);
  const trySend = (msg: string) => {
    if (socket.readyState == socket.OPEN) {
      socket.send(msg);
    }
  };

  socket.addEventListener("open", () => {
    trySend("open=");
  });

  const timeouts = new Set<number>();
  socket.addEventListener("message", (event) => {
    if (typeof event.data !== "string") return;

    const [, topic] = event.data.match(/^sub=(.+)$/) ?? [];
    if (!topic) return;
    let timeoutId = -1;
    let n = 0;
    const ping = () => {
      timeouts.delete(timeoutId);
      n++;
      trySend(`event=topic=${topic} n=${n}`);
      timeoutId = setTimeout(ping, 1000);
      timeouts.add(timeoutId);
    };
    ping();
  });
  setTimeout(() => {
    trySend("event=reconnect!");
    trySend(`reconnect=ws://localhost:8080`);
    for (const id of timeouts) {
      clearTimeout(id);
    }
    socket.close();
  }, 10_000);

  return response;
});
