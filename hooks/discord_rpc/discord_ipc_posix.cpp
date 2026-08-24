#include "discord_ipc.h"

#include <atomic>
#include <chrono>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>

#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#include <csignal>
#include <cerrno>

#pragma mark - Logging

static FILE *gLog = NULL;
static void logmsg(const char *fmt, ...) {
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

#pragma mark - Wire protocol (opcodes, framing, JSON)

// Discord's local IPC protocol: an 8-byte header (uint32 LE opcode,
// uint32 LE payload length) followed by that many bytes of UTF-8 JSON.
// HANDSHAKE=0 is sent once with {v, client_id}; FRAME=1 carries every
// subsequent request (SET_ACTIVITY here) as a small JSON-RPC-shaped
// payload. Nothing beyond these two opcodes is needed for Rich Presence.
enum { OP_HANDSHAKE = 0, OP_FRAME = 1 };

static bool writeAll(int fd, const void *data, size_t n) {
    const uint8_t *p = (const uint8_t *)data;
    size_t total = 0;
    while (total < n) {
        ssize_t w = write(fd, p + total, n - total);
        if (w <= 0) return false;
        total += (size_t)w;
    }
    return true;
}

static bool writeFrame(int fd, uint32_t opcode, const std::string &json) {
    uint32_t len = (uint32_t)json.size();
    uint8_t header[8];
    memcpy(header, &opcode, 4);
    memcpy(header + 4, &len, 4);
    return writeAll(fd, header, sizeof(header)) && writeAll(fd, json.data(), json.size());
}

static bool readFrame(int fd, uint32_t *opOut, std::string *jsonOut) {
    uint8_t header[8];
    size_t total = 0;
    while (total < sizeof(header)) {
        ssize_t n = read(fd, header + total, sizeof(header) - total);
        if (n <= 0) return false;
        total += (size_t)n;
    }
    uint32_t op = 0, len = 0;
    memcpy(&op, header, 4);
    memcpy(&len, header + 4, 4);
    if (len > (1u << 20)) return false; // sanity cap, real payloads are tiny
    jsonOut->resize(len);
    total = 0;
    while (total < len) {
        ssize_t n = read(fd, &(*jsonOut)[total], len - total);
        if (n <= 0) return false;
        total += (size_t)n;
    }
    *opOut = op;
    return true;
}

static std::string jsonEscape(const std::string &s) {
    std::string out;
    out.reserve(s.size() + 8);
    for (unsigned char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (c < 0x20) {
                    char buf[8];
                    snprintf(buf, sizeof(buf), "\\u%04x", c);
                    out += buf;
                } else {
                    out += (char)c;
                }
        }
    }
    return out;
}

#pragma mark - Connection

// Discord listens on a Unix socket named discord-ipc-{0..9} (0 for the
// primary client, higher numbers for Canary/PTB/multiple accounts) inside
// whichever of these directories it picked at startup - checked in the
// same order Discord's own clients document.
static bool tryConnectOnce(int *outFd) {
    const char *candidateDirs[3] = {getenv("XDG_RUNTIME_DIR"), getenv("TMPDIR"), "/tmp"};
    for (const char *dir : candidateDirs) {
        if (!dir || !dir[0]) continue;
        for (int i = 0; i < 10; i++) {
            char path[1024];
            snprintf(path, sizeof(path), "%s/discord-ipc-%d", dir, i);

            int fd = socket(AF_UNIX, SOCK_STREAM, 0);
            if (fd < 0) continue;

            struct timeval tv = {2, 0};
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

            struct sockaddr_un addr;
            memset(&addr, 0, sizeof(addr));
            addr.sun_family = AF_UNIX;
            strncpy(addr.sun_path, path, sizeof(addr.sun_path) - 1);

            if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) == 0) {
                *outFd = fd;
                return true;
            }
            close(fd);
        }
    }
    return false;
}

static bool doHandshake(int fd, const std::string &clientId) {
    std::string payload = "{\"v\":1,\"client_id\":\"" + jsonEscape(clientId) + "\"}";
    if (!writeFrame(fd, OP_HANDSHAKE, payload)) return false;
    uint32_t op = 0;
    std::string resp;
    if (!readFrame(fd, &op, &resp)) return false; // any well-formed reply confirms the socket is really Discord
    logmsg("handshake ok, response: %s\n", resp.c_str());
    return true;
}

static bool sendActivity(int fd, const std::string &state, const std::string &details, int64_t startUnix,
                          const std::string &largeImageKey, const std::string &largeImageText,
                          const std::string &smallImageKey, const std::string &smallImageText,
                          const std::string &buttonLabel, const std::string &buttonUrl) {
    static std::atomic<uint64_t> gNonce{0};
    std::string assets;
    if (!largeImageKey.empty() || !smallImageKey.empty()) {
        assets = ",\"assets\":{";
        bool needComma = false;
        if (!largeImageKey.empty()) {
            assets += "\"large_image\":\"" + jsonEscape(largeImageKey) + "\"";
            needComma = true;
            if (!largeImageText.empty()) assets += ",\"large_text\":\"" + jsonEscape(largeImageText) + "\"";
        }
        if (!smallImageKey.empty()) {
            if (needComma) assets += ",";
            assets += "\"small_image\":\"" + jsonEscape(smallImageKey) + "\"";
            if (!smallImageText.empty()) assets += ",\"small_text\":\"" + jsonEscape(smallImageText) + "\"";
        }
        assets += "}";
    }
    std::string buttons;
    if (!buttonLabel.empty() && !buttonUrl.empty()) {
        buttons = ",\"buttons\":[{\"label\":\"" + jsonEscape(buttonLabel) + "\",\"url\":\"" + jsonEscape(buttonUrl) + "\"}]";
    }
    char payload[4096];
    snprintf(payload, sizeof(payload),
        "{\"cmd\":\"SET_ACTIVITY\",\"args\":{\"pid\":%d,\"activity\":{\"state\":\"%s\",\"details\":\"%s\","
        "\"timestamps\":{\"start\":%lld}%s%s}},\"nonce\":\"%llu\"}",
        (int)getpid(), jsonEscape(state).c_str(), jsonEscape(details).c_str(),
        (long long)startUnix, assets.c_str(), buttons.c_str(), (unsigned long long)gNonce.fetch_add(1));
    return writeFrame(fd, OP_FRAME, payload);
}

#pragma mark - Background thread + pending-activity queue

#define RECONNECT_INTERVAL_MS 5000
#define IDLE_POLL_MS 200

static std::mutex gMutex;
static std::string gPendingState, gPendingDetails;
static std::string gPendingLargeImageKey, gPendingLargeImageText;
static std::string gPendingSmallImageKey, gPendingSmallImageText;
static std::string gPendingButtonLabel, gPendingButtonUrl;
static int64_t gPendingStart = 0;
static std::atomic<bool> gHasPendingActivity{false};

static void ipcLoop(std::string clientId) {
    gLog = fopen("/tmp/studio_patcher_discord_ipc.txt", "w");
    signal(SIGPIPE, SIG_IGN); // writing to a socket Discord already closed must not kill the host process

    int fd = -1;
    for (;;) {
        if (fd < 0) {
            int candidate;
            if (tryConnectOnce(&candidate)) {
                if (doHandshake(candidate, clientId)) {
                    fd = candidate;
                    logmsg("connected, fd=%d\n", fd);
                } else {
                    close(candidate);
                }
            }
        }

        if (fd >= 0 && gHasPendingActivity.load(std::memory_order_acquire)) {
            std::string state, details, largeImageKey, largeImageText, smallImageKey, smallImageText;
            std::string buttonLabel, buttonUrl;
            int64_t start;
            {
                std::lock_guard<std::mutex> lock(gMutex);
                state = gPendingState;
                details = gPendingDetails;
                start = gPendingStart;
                largeImageKey = gPendingLargeImageKey;
                largeImageText = gPendingLargeImageText;
                smallImageKey = gPendingSmallImageKey;
                smallImageText = gPendingSmallImageText;
                buttonLabel = gPendingButtonLabel;
                buttonUrl = gPendingButtonUrl;
            }
            gHasPendingActivity.store(false, std::memory_order_release);

            if (!sendActivity(fd, state, details, start, largeImageKey, largeImageText, smallImageKey, smallImageText,
                               buttonLabel, buttonUrl)) {
                logmsg("send failed, will reconnect: %s\n", strerror(errno));
                close(fd);
                fd = -1;
            } else {
                logmsg("sent activity: state=\"%s\" details=\"%s\" largeImage=\"%s\" smallImage=\"%s\" button=\"%s\"->%s\n",
                       state.c_str(), details.c_str(), largeImageKey.c_str(), smallImageKey.c_str(),
                       buttonLabel.c_str(), buttonUrl.c_str());
            }
        }

        std::this_thread::sleep_for(std::chrono::milliseconds(fd < 0 ? RECONNECT_INTERVAL_MS : IDLE_POLL_MS));
    }
}

void DiscordIpc_Start(const char *clientId) {
    static std::thread worker(ipcLoop, std::string(clientId));
    worker.detach();
}

void DiscordIpc_SetActivity(const char *state, const char *details, int64_t startTimestampUnix,
                             const char *largeImageKey, const char *largeImageText,
                             const char *smallImageKey, const char *smallImageText,
                             const char *buttonLabel, const char *buttonUrl) {
    std::lock_guard<std::mutex> lock(gMutex);
    gPendingState = state ? state : "";
    gPendingDetails = details ? details : "";
    gPendingStart = startTimestampUnix;
    gPendingLargeImageKey = largeImageKey ? largeImageKey : "";
    gPendingLargeImageText = largeImageText ? largeImageText : "";
    gPendingSmallImageKey = smallImageKey ? smallImageKey : "";
    gPendingSmallImageText = smallImageText ? smallImageText : "";
    gPendingButtonLabel = buttonLabel ? buttonLabel : "";
    gPendingButtonUrl = buttonUrl ? buttonUrl : "";
    gHasPendingActivity.store(true, std::memory_order_release);
}
