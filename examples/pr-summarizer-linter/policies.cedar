// policies.cedar — who may do what, and through which gateway. Nothing here says how narrowly
// the broker executes it: timeouts, hosts, methods, call budgets, and credential binding live in
// broker.yaml's constraint sets. Editing policy may broaden who can act; it cannot reach a new
// host or bind authority that was not already constrained.
//
// chmod 0600, owner-owned, single-link. Cedar validates this in strict mode at startup against a
// schema generated from broker.yaml's declared world, so a typo in a principal, provider, or
// capability refuses startup instead of becoming a rule that silently never matches.
//

// Session gate: may this person drive this agent at all, and through which gateway? This is checked
// before `capabilitiesFor` answers, so an unauthorized sender is refused before a model call costs
// anything.
@id("maintainer-may-prompt-pr-summarizer-linter")
permit(principal == Dekopon::Principal::"maintainer",
       action == Dekopon::Action::"agent.prompt",
       resource == Dekopon::Agent::"pr-summarizer-linter")
when { context has via && context.via == "dekopon-gatewayd"
    && context has transportKind && context.transportKind == "slack"
    && context has transport && context.transport == "workspace-slack" };

// Capability surface: driving the agent implies none of these actions, and holding these actions
// implies no session. Command shorthands propose these same capability actions. `context.via` keeps attested and direct authority disjoint; `context.agent`
// pins the surface to this one agent. Approval, request-changes, merge, and issue-comment lifecycle writes are deliberately absent.
// The COMMENT action includes optional pinned line comments under the same two-request grant.
@id("pr-summarizer-linter-gh-surface")
permit(principal == Dekopon::Principal::"maintainer",
       action in [Dekopon::Action::"gh.content.read",
                  Dekopon::Action::"gh.pull-request.read",
                  Dekopon::Action::"gh.pull-request.diff",
                  Dekopon::Action::"gh.pull-request.status",
                  Dekopon::Action::"gh.run.list",
                  Dekopon::Action::"gh.run.read",
                  Dekopon::Action::"gh.actions.logs.read",
                  Dekopon::Action::"gh.pull-request.comment"],
       resource == Dekopon::Provider::"gh")
when { context has agent && context.agent == "pr-summarizer-linter"
    && context has via && context.via == "dekopon-gatewayd"
    && context has transportKind && context.transportKind == "slack"
    && context has transport && context.transport == "workspace-slack" };
