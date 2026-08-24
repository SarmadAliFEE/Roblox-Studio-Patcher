// Resolves a Roblox PlaceId to a friendly place name and thumbnail image
// URL. Deliberately implemented as native HTTPS calls to Roblox's public
// REST API instead of routing through MarketplaceService in the VM -
// that's the "correct" Roblox-side API for this, but it yields on an HTTP
// request, and this project's raw call-dispatch context has no safe way
// to let a Luau call yield (task.spawn itself errored and likely
// corrupted VM scheduler state trying - see project memory). This gets
// the same end result (name + thumbnail) with zero VM risk.
#pragma once

#include <string>

// Kicks off (if not already in flight for this exact placeId) an async
// lookup on a background thread. Safe to call on every poll tick - cheap
// no-op once a result for this placeId is cached or a fetch is already
// running.
//
// Returns true and fills outName/outThumbnailUrl only once a result is
// actually available for this exact placeId (may take one or more ticks -
// the real network fetch happens off-thread). Returns false while a fetch
// is still in flight; callers should fall back to something derived
// purely from placeId in the meantime. A failed lookup still eventually
// resolves (to a "Place <id>" fallback name, no thumbnail) rather than
// retrying forever.
//
// outIsPublic reflects whether games.roblox.com actually returned real
// game info for this placeId's universe, unauthenticated - which is
// exactly what a private/unpublished place fails to do (an empty `data`
// array, not an error), so this doubles as "is this place joinable by
// whoever sees the Rich Presence" without any separate privacy check.
// Only meaningful when this call returns true.
bool DiscordPlaceLookup_Get(const std::string &placeId, std::string &outName, std::string &outThumbnailUrl,
                             bool &outIsPublic);
