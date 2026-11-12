//go:build unix

package ingestion

import "syscall"

const oNoFollow = syscall.O_NOFOLLOW
