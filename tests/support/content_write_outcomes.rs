pub struct Case {
    pub http_status: u16,
    pub exit_status: u8,
    pub stderr: &'static str,
}

pub const CASES: [Case; 5] = [
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
        http_status: 0,
        exit_status: 5,
        stderr: "file write outcome is unknown; read state before deciding what to do next\n",
    },
];
