pub struct Case {
    pub http_status: u16,
    pub exit_status: u8,
    pub stderr: &'static str,
}

pub const UNKNOWN_STDERR: &str =
    "file write outcome is unknown; read state before deciding what to do next\n";

pub const CASES: [Case; 13] = [
    Case {
        http_status: 409,
        exit_status: 4,
        stderr: "file write refused: content conflict or repository rule violation\n",
    },
    Case {
        http_status: 403,
        exit_status: 3,
        stderr: "credential is not permitted to make this call\n",
    },
    Case {
        http_status: 404,
        exit_status: 3,
        stderr: "not found, or not visible to this credential\n",
    },
    Case {
        http_status: 422,
        exit_status: 3,
        stderr: "endpoint refused the request as invalid\n",
    },
    Case {
        http_status: 408,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 499,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 500,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 502,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 503,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 504,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 202,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 302,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
    Case {
        http_status: 0,
        exit_status: 5,
        stderr: UNKNOWN_STDERR,
    },
];

pub fn unreadable_replies() -> [String; 3] {
    [
        "{}".into(),
        "{".into(),
        serde_json::json!({"content":{"path":"config/mutable/other.yaml","type":"file","sha":"b".repeat(40)},"commit":{"sha":"c".repeat(40)}}).to_string(),
    ]
}
