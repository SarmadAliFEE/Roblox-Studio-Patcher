#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod arch {
    pub const STEP: &str = concat!(
        "ff 43 03 d1 f6 57 0a a9 f4 4f 0b a9 fd 7b 0c a9 ",
        "fd 03 03 91 f3 03 01 aa f4 03 00 aa 08 00 46 39 ",
        "a8 00 00 37 88 c6 40 f9 08 01 1b 91 08 fd df 08 ",
        "?? ?? ?? ?? 80 c6 40 f9 a8 03 01 d1 ",
        "?? ?? ?? ?? ?? ?? ?? ??",
    );

    pub const LUAU_LOAD_WRAPPER: &str = concat!(
        "ff 83 02 d1 fa 67 05 a9 f8 5f 06 a9 f6 57 07 a9 ",
        "f4 4f 08 a9 fd 7b 09 a9 fd 43 02 91 f4 03 04 aa ",
        "f5 03 03 aa f6 03 02 aa f7 03 01 aa f3 03 00 aa ",
        "18 18 40 f9 19 23 45 a9 1f 01 19 eb c3 00 00 54",
    );

    pub const CALL_DISPATCH: &str = concat!(
        "f6 57 bd a9 f4 4f 01 a9 fd 7b 02 a9 fd 83 00 91 ",
        "f4 03 02 aa f3 03 00 aa ?? ?? ?? ?? f5 03 00 aa ",
        "60 02 00 35 68 1a 40 f9 08 95 42 f9 a8 02 00 b5 ",
        "75 12 40 79 68 2e 40 f9 02 d1 34 cb ?? ?? ?? ??",
    );

    pub const TASK_DEFER: &str = concat!(
        "ff 83 03 d1 f8 5f 0a a9 f6 57 0b a9 f4 4f 0c a9 ",
        "fd 7b 0d a9 fd 43 03 91 f3 03 00 aa ?? ?? ?? ?? ",
        "?? ?? ?? ?? 08 01 00 37 ?? ?? ?? ??",
    );

    pub const LUA_NEWTHREAD: &str = concat!(
        "f4 4f be a9 fd 7b 01 a9 fd 43 00 91 f3 03 00 aa ",
        "08 18 40 f9 08 25 45 a9 3f 01 08 eb 83 00 00 54 ",
        "e0 03 13 aa 21 00 80 52 ?? ?? ?? ??",
    );

    pub const CAN_ACCESS_RESTRICTED: &str = concat!(
        "f6 57 bd a9 f4 4f 01 a9 fd 7b 02 a9 fd 83 00 91 ",
        "f4 03 00 aa 15 0c 40 f9 ?? ?? ?? ?? f3 03 00 aa ",
        "b5 42 46 39 35 01 00 b4 60 a2 42 a9 88 00 00 b4 ",
        "61 0e 40 f9 00 01 3f d6 60 fe 02 a9 e8 03 20 2a ",
        "1f 01 15 ea a1 01 00 54 94 ae 42 39 34 01 00 b4 ",
        "60 a2 42 a9 88 00 00 b4 61 0e 40 f9 00 01 3f d6 ",
        "60 fe 02 a9 e8 03 20 2a 1f 01 14 ea 61 00 00 54 ",
        "20 00 80 52 02 00 00 14 00 00 80 52 fd 7b 42 a9 ",
        "f4 4f 41 a9 f6 57 c3 a8 c0 03 5f d6",
    );

    pub const CAN_ACCESS_RESTRICTED_BL: usize = 0x18;
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
mod arch {
    pub const STEP: &str = concat!(
        "48 89 5c 24 10 48 89 74 24 18 55 57 41 56 48 8d ",
        "6c 24 b9 48 81 ec c0 00 00 00 4c 8b f2 48 8b f1 ",
        "80 b9 90 01 00 00 00 75 1d 48 8b 81 98 01 00 00 ",
        "0f b6 80 50 07 00 00",
    );

    pub const LUAU_LOAD_WRAPPER: &str = concat!(
        "48 89 5c 24 08 48 89 6c 24 10 48 89 74 24 18 57 ",
        "41 56 41 57 48 81 ec 80 00 00 00 49 8b e9 4d 8b ",
        "f0 4c 8b fa 48 8b f9 48 8b 59 30 48 8b 43 50 48 ",
        "39 43 58 72",
    );

    pub const CALL_DISPATCH: &str = concat!(
        "48 89 74 24 10 57 48 83 ec 20 49 63 f0 48 8b f9 ",
        "44 8b c6 e8 ?? ?? ?? ?? 85 c0 75 66 48 8b 47 30 ",
        "48 89 5c 24 30 48 8b 90 28 05 00 00 48 85 d2 74 ",
        "05 48 8b cf ff d2 4c 8b 47 58",
    );

    pub const TASK_DEFER: &str = "";
    pub const LUA_NEWTHREAD: &str = concat!(
        "40 53 48 83 ec 20 48 8b 51 30 48 8b d9 48 8b 42 ",
        "50 48 39 42 58 72 07 b2 01 e8 ?? ?? ?? ?? f6 43 ",
        "01 04 74",
    );
    pub const CAN_ACCESS_RESTRICTED: &str = concat!(
        "48 89 5c 24 60 e8 ?? ?? ?? ?? 48 8b d8 48 8b 40 ",
        "30 48 85 c0 74 16 48 8b 53 18 48 8b 4b 28 ff d0 ",
        "48 89 43 28 48 c7 43 30 00 00 00 00 0f b6 43 28",
    );
    pub const CAN_ACCESS_RESTRICTED_BL: usize = 0x5;
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod arch {
    pub const STEP: &str = concat!(
        "55 48 89 e5 41 57 41 56 41 54 53 48 81 ec a0 00 ",
        "00 00 48 89 f3 49 89 fe 80 bf 80 01 00 00 00 75 ",
        "1b 49 8b 86 88 01 00 00",
    );

    pub const LUAU_LOAD_WRAPPER: &str = concat!(
        "55 48 89 e5 41 57 41 56 41 55 41 54 53 48 83 ec ",
        "58 49 89 d5 49 89 f7 48 89 fb 4c 8b 67 30 49 8b ",
        "44 24 50 49 39 44 24 58 72 22 48 89 df",
    );

    pub const CALL_DISPATCH: &str = concat!(
        "55 48 89 e5 41 57 41 56 53 50 41 89 d6 48 89 fb ",
        "e8 ?? ?? ?? ?? 41 89 c7 85 c0 75 52 48 8b 43 30 ",
        "48 8b 80 28 05 00 00 48 85 c0",
    );

    pub const TASK_DEFER: &str = "";

    pub const LUA_NEWTHREAD: &str = concat!(
        "55 48 89 e5 41 56 53 48 89 fb 48 8b 47 30 48 8b ",
        "48 58 48 3b 48 50 72 0d 48 89 df be 01 00 00 00 ",
        "e8 ?? ?? ?? ?? f6 43 01 04",
    );

    pub const CAN_ACCESS_RESTRICTED: &str = concat!(
        "48 89 e5 41 57 41 56 53 50 49 89 fe 4c 8b 7f 18 ",
        "e8 ?? ?? ?? ?? 48 89 c3 45 0f b6 bf 90 01 00 00 ",
        "4d 85 ff 74 29 48 8b 43 28 48 8b 4b 30",
    );

    pub const CAN_ACCESS_RESTRICTED_BL: usize = 0x10;
}

pub use arch::*;

#[cfg(target_os = "macos")]
pub const DATA_MODEL_RTTI: &str = "N3RBX9DataModelE";
#[cfg(target_os = "macos")]
pub const SCRIPT_CONTEXT_RTTI: &str = "N3RBX13ScriptContextE";
#[cfg(target_os = "macos")]
pub const WAITING_HYBRID_RTTI: &str = "N3RBX19ScriptContextFacets23WaitingHybridScriptsJobE";
#[cfg(target_os = "macos")]
pub const JOB_CLASSES: &[&str] = &[
    "N3RBX12DataModelJobE",
    "N3RBX19GenericDataModelJobE",
    "N3RBX13TaskScheduler3JobE",
    "N3RBX9DataModel10GenericJobE",
    WAITING_HYBRID_RTTI,
];

#[cfg(target_os = "windows")]
pub const DATA_MODEL_RTTI: &str = ".?AVDataModel@RBX@@";
#[cfg(target_os = "windows")]
pub const SCRIPT_CONTEXT_RTTI: &str = ".?AVScriptContext@RBX@@";
#[cfg(target_os = "windows")]
pub const WAITING_HYBRID_RTTI: &str = ".?AVWaitingHybridScriptsJob@ScriptContextFacets@RBX@@";
#[cfg(target_os = "windows")]
pub const JOB_CLASSES: &[&str] = &[
    ".?AVDataModelJob@RBX@@",
    ".?AVGenericDataModelJob@RBX@@",
    ".?AVJob@TaskScheduler@RBX@@",
    ".?AVGenericJob@DataModel@RBX@@",
    WAITING_HYBRID_RTTI,
];
