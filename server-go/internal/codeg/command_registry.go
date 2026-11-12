package codeg

// Dispatch registry.
//
// Historically every backend endpoint was a `case` in the giant store-locked
// switch inside command(), so ANY backend change — new feature or fix — had to
// edit server.go. In a multi-agent workflow that made server.go a guaranteed
// conflict hotspot. The registry lets each feature domain live in its own file
// and register its handlers here; adding an endpoint no longer touches
// server.go, so parallel work lands in disjoint files.
//
// lockedCmd mirrors the contract of a case in command()'s store-locked section:
// it runs with the workspace store mutex ALREADY held, and may therefore use
// the *Store *Locked helpers and saveLocked() directly. command() looks a
// command up here before falling through to the (shrinking) inline switch, so
// migrated and not-yet-migrated commands coexist without behavior change.
type lockedCmd func(s *Server, st *Store, command string, a Row) (any, error)

var lockedCommands = map[string]lockedCmd{}

// registerLocked maps one or more command names to a store-locked handler. Call
// it from a domain file's init(). Re-registering a name overrides it, which is
// how a command is "moved" out of the inline switch: register it here and
// delete its old case.
func registerLocked(fn lockedCmd, names ...string) {
	for _, n := range names {
		lockedCommands[n] = fn
	}
}
