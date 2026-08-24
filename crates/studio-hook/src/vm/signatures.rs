#[cfg(target_arch = "aarch64")]
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

pub use arch::*;

pub const DATA_MODEL_RTTI: &str = "N3RBX9DataModelE";
pub const SCRIPT_CONTEXT_RTTI: &str = "N3RBX13ScriptContextE";
pub const WAITING_HYBRID_RTTI: &str = "N3RBX19ScriptContextFacets23WaitingHybridScriptsJobE";

pub const JOB_CLASSES: &[&str] = &[
    "N3RBX12DataModelJobE",
    "N3RBX19GenericDataModelJobE",
    "N3RBX13TaskScheduler3JobE",
    "N3RBX9DataModel10GenericJobE",
    WAITING_HYBRID_RTTI,
];
