@id("run-file-agent")
permit(principal == Dekopon::Principal::"content-writer",
       action == Dekopon::Action::"agent.prompt",
       resource == Dekopon::Agent::"content-writer")
when { context.via == "content-write-gateway" };

@id("run-file-write")
permit(principal == Dekopon::Principal::"content-writer",
       action == Dekopon::Action::"gh.content.write",
       resource == Dekopon::Provider::"gh")
when { context.via == "content-write-gateway" && context.agent == "content-writer" };

@id("run-file-write-fence")
forbid(principal, action == Dekopon::Action::"gh.content.write", resource)
when { context has request }
unless { context.request.method == "PUT"
    && context.request.url.host == "api.github.com"
    && context.request.url.path == "/repos/octo/config/contents/config/mutable/agent.yaml"
    && !(context.request.url has query)
    && context.request has body
    && context.request.body has branch
    && context.request.body.branch == "runs/content-123" };

@id("run-file-read")
permit(principal == Dekopon::Principal::"content-writer",
       action == Dekopon::Action::"gh.content.read",
       resource == Dekopon::Provider::"gh")
when { context.via == "content-write-gateway" && context.agent == "content-writer" };

@id("run-file-read-fence")
forbid(principal, action == Dekopon::Action::"gh.content.read", resource)
when { context has request }
unless { context.request.method == "GET"
    && context.request.url.host == "api.github.com"
    && context.request.url.path == "/repos/octo/config/contents/config/mutable/agent.yaml"
    && context.request.url has query
    && context.request.url.query == "ref=runs%2Fcontent-123" };
