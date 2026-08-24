#import <Foundation/Foundation.h>
#include "discord_place_lookup.h"

#include <chrono>
#include <cstdarg>
#include <cstdio>
#include <mutex>
#include <string>

static uint64_t monotonicMillis(void) {
    return (uint64_t)std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count();
}

static FILE *gLog = NULL;
static void logmsg(const char *fmt, ...) {
    if (!gLog) gLog = fopen("/tmp/studio_patcher_discord_place_lookup.txt", "w");
    if (!gLog) return;
    va_list args;
    va_start(args, fmt);
    vfprintf(gLog, fmt, args);
    va_end(args);
    fflush(gLog);
}

#define RESOLVE_TTL_MS 60000

static std::mutex gMutex;
static std::string gInFlightPlaceId;
static std::string gResolvedPlaceId;
static std::string gResolvedName;
static std::string gResolvedThumbnailUrl;
static bool gResolvedIsPublic = false;
static bool gResolvedReady = false;
static uint64_t gResolvedAtMs = 0;

static void storeResolved(const std::string &placeId, const std::string &name, const std::string &thumbnailUrl,
                           bool isPublic) {
    std::lock_guard<std::mutex> lock(gMutex);
    gResolvedPlaceId = placeId;
    gResolvedName = name;
    gResolvedThumbnailUrl = thumbnailUrl;
    gResolvedIsPublic = isPublic;
    gResolvedReady = true;
    gResolvedAtMs = monotonicMillis();
    if (gInFlightPlaceId == placeId) gInFlightPlaceId.clear();
}

static NSURLSession *anonymousSession(void) {
    static NSURLSession *session = nil;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        NSURLSessionConfiguration *config = [NSURLSessionConfiguration ephemeralSessionConfiguration];
        config.HTTPShouldSetCookies = NO;
        config.HTTPCookieAcceptPolicy = NSHTTPCookieAcceptPolicyNever;
        config.HTTPCookieStorage = nil;
        config.URLCredentialStorage = nil;
        session = [NSURLSession sessionWithConfiguration:config];
    });
    return session;
}

static NSString *jsonString(id obj, NSString *key) {
    if (![obj isKindOfClass:[NSDictionary class]]) return nil;
    id v = ((NSDictionary *)obj)[key];
    return [v isKindOfClass:[NSString class]] ? v : nil;
}

static bool jsonBool(id obj, NSString *key, bool defaultValue) {
    if (![obj isKindOfClass:[NSDictionary class]]) return defaultValue;
    id v = ((NSDictionary *)obj)[key];
    return [v isKindOfClass:[NSNumber class]] ? ((NSNumber *)v).boolValue : defaultValue;
}

static id firstDataEntry(NSData *data) {
    if (!data) return nil;
    id obj = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    if (![obj isKindOfClass:[NSDictionary class]]) return nil;
    NSArray *arr = ((NSDictionary *)obj)[@"data"];
    if (![arr isKindOfClass:[NSArray class]] || arr.count == 0) return nil;
    return arr[0];
}

static void fetchThumbnail(NSString *universeId, std::string placeId, std::string name, bool isPublic) {
    NSString *urlStr = [NSString stringWithFormat:
        @"https://thumbnails.roblox.com/v1/games/icons?universeIds=%@&returnPolicy=PlaceHolder&size=512x512&format=Png&isCircular=false",
        universeId];
    NSURLSessionDataTask *task = [anonymousSession()
        dataTaskWithURL:[NSURL URLWithString:urlStr]
        completionHandler:^(NSData *data, NSURLResponse *response, NSError *error) {
            std::string thumbnailUrl;
            NSString *imgUrl = jsonString(firstDataEntry(data), @"imageUrl");
            if (imgUrl) thumbnailUrl = imgUrl.UTF8String;
            logmsg("resolved place %s -> name=\"%s\" thumbnail=%s isPublic=%d\n", placeId.c_str(), name.c_str(),
                   thumbnailUrl.empty() ? "(none)" : thumbnailUrl.c_str(), (int)isPublic);
            storeResolved(placeId, name, thumbnailUrl, isPublic);
        }];
    [task resume];
}

static void checkPublicity(NSString *universeId, std::string placeId, std::string displayName) {
    NSString *urlStr = [NSString stringWithFormat:@"https://games.roblox.com/v1/games?universeIds=%@", universeId];
    NSURLSessionDataTask *task = [anonymousSession()
        dataTaskWithURL:[NSURL URLWithString:urlStr]
        completionHandler:^(NSData *data, NSURLResponse *response, NSError *error) {
            id entry = firstDataEntry(data);
            bool isPublic = entry && !jsonBool(entry, @"isContentRestricted", true);
            fetchThumbnail(universeId, placeId, displayName, isPublic);
        }];
    [task resume];
}

static void fetchGameInfo(NSString *universeId, std::string placeId) {
    NSString *urlStr = [NSString stringWithFormat:@"https://games.roblox.com/v1/games?universeIds=%@", universeId];
    NSURLSessionDataTask *task = [[NSURLSession sharedSession]
        dataTaskWithURL:[NSURL URLWithString:urlStr]
        completionHandler:^(NSData *data, NSURLResponse *response, NSError *error) {
            NSString *name = jsonString(firstDataEntry(data), @"name");
            std::string displayName = "Place " + placeId;
            if (name && ![name isEqualToString:@"[TITLE UNAVAILABLE]"]) {
                displayName = name.UTF8String;
            } else {
                logmsg("game info lookup gave no usable name for placeId %s\n", placeId.c_str());
            }
            checkPublicity(universeId, placeId, displayName);
        }];
    [task resume];
}

static void startFetch(std::string placeId) {
    NSString *placeIdStr = [NSString stringWithUTF8String:placeId.c_str()];
    NSString *urlStr = [NSString stringWithFormat:@"https://apis.roblox.com/universes/v1/places/%@/universe", placeIdStr];
    NSURLSessionDataTask *task = [[NSURLSession sharedSession]
        dataTaskWithURL:[NSURL URLWithString:urlStr]
        completionHandler:^(NSData *data, NSURLResponse *response, NSError *error) {
            id obj = data ? [NSJSONSerialization JSONObjectWithData:data options:0 error:nil] : nil;
            id uid = [obj isKindOfClass:[NSDictionary class]] ? ((NSDictionary *)obj)[@"universeId"] : nil;
            if (![uid isKindOfClass:[NSNumber class]]) {
                logmsg("universe lookup failed for placeId %s\n", placeId.c_str());
                storeResolved(placeId, "Place " + placeId, "", false);
                return;
            }
            fetchGameInfo([(NSNumber *)uid stringValue], placeId);
        }];
    [task resume];
}

bool DiscordPlaceLookup_Get(const std::string &placeId, std::string &outName, std::string &outThumbnailUrl,
                             bool &outIsPublic) {
    bool shouldStartFetch = false;
    bool haveCached = false;
    {
        std::lock_guard<std::mutex> lock(gMutex);
        if (gResolvedReady && gResolvedPlaceId == placeId) {
            outName = gResolvedName;
            outThumbnailUrl = gResolvedThumbnailUrl;
            outIsPublic = gResolvedIsPublic;
            haveCached = true;
            if (monotonicMillis() - gResolvedAtMs >= RESOLVE_TTL_MS && gInFlightPlaceId != placeId) {
                gInFlightPlaceId = placeId;
                shouldStartFetch = true;
            }
        } else if (gInFlightPlaceId != placeId) {
            gInFlightPlaceId = placeId;
            gResolvedReady = false;
            shouldStartFetch = true;
        }
    }
    if (shouldStartFetch) startFetch(placeId);
    return haveCached;
}
