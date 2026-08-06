#!/usr/bin/env bash
kept=()
for arg in "$@"; do
    [ "$arg" = "-lstdc++" ] && continue
    kept+=("$arg")
done

exec x86_64-w64-mingw32-g++ \
    -Wl,--start-group "${kept[@]}" \
    -static-libgcc -static-libstdc++ \
    -Wl,-Bstatic -lstdc++ -lwinpthread -lmingwex -Wl,-Bdynamic \
    -lkernel32 -luser32 -ladvapi32 -lmsvcrt -Wl,--end-group
