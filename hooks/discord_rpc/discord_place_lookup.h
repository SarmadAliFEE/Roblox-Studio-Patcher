#pragma once

#include <string>

bool DiscordPlaceLookup_Get(const std::string &placeId, std::string &outName, std::string &outThumbnailUrl,
                             bool &outIsPublic);
