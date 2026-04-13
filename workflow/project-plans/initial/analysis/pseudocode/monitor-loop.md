# Pseudocode: Monitor Supervision Loop

1. Acquire singleton lock for configured monitor scope.
2. Spawn engine instance for selected run/profile.
3. Publish heartbeat/status metadata for CLI/service visibility.
4. Wait on engine exit, monitor command, or shutdown signal.
5. If command is `status`, return current monitor/engine state.
6. If command is `shutdown`, request graceful engine stop and persist final monitor state.
7. If engine exits unexpectedly, increment restart counter.
8. If counter exceeds configured limit, mark monitor degraded/unhealthy and stop restart loop.
9. Apply configured backoff before restart attempt.
10. Restart engine when allowed.
11. Continue heartbeat updates while monitor active.
