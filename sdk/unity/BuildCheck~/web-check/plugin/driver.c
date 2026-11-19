// Stands where the C# of the package stands in a Web build: calls the plug-in and prints what
// the plug-in calls back with, one line per call.
#include <emscripten.h>
#include <stdio.h>

int AurixNetOpen(const char *url, void (*on_open)(int), void (*on_message)(int, const unsigned char *, int, int), void (*on_close)(int, int));
void AurixNetSend(int socket, const unsigned char *data, int length, int text);
void AurixNetClose(int socket);
int AurixNetEvery(int ms, void (*on_timer)(int));
int AurixNetAfter(int ms, void (*on_timer)(int));
void AurixNetCancel(int timer);

static void on_open(int socket) { printf("open %d\n", socket); }

static void on_message(int socket, const unsigned char *data, int length, int text)
{
    printf("message %d %s ", socket, text ? "text" : "binary");
    for (int i = 0; i < length; i++) printf("%02x", data[i]);
    printf("\n");
}

static void on_close(int socket, int code) { printf("close %d %d\n", socket, code); }

static void on_timer(int timer) { printf("timer %d\n", timer); }

EMSCRIPTEN_KEEPALIVE int driver_open(const char *url) { return AurixNetOpen(url, on_open, on_message, on_close); }
EMSCRIPTEN_KEEPALIVE void driver_send(int socket, const unsigned char *data, int length, int text) { AurixNetSend(socket, data, length, text); }
EMSCRIPTEN_KEEPALIVE void driver_close(int socket) { AurixNetClose(socket); }
EMSCRIPTEN_KEEPALIVE int driver_every(int ms) { return AurixNetEvery(ms, on_timer); }
EMSCRIPTEN_KEEPALIVE int driver_after(int ms) { return AurixNetAfter(ms, on_timer); }
EMSCRIPTEN_KEEPALIVE void driver_cancel(int timer) { AurixNetCancel(timer); }
