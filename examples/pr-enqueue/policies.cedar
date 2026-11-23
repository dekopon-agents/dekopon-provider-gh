@id("enqueue-agent")
permit(principal == Dekopon::Principal::"queue-writer",
       action == Dekopon::Action::"agent.prompt",
       resource == Dekopon::Agent::"queue-writer")
when { context has via && context.via == "queue-gateway" };

@id("enqueue")
permit(principal == Dekopon::Principal::"queue-writer",
       action == Dekopon::Action::"gh.pull-request.enqueue",
       resource == Dekopon::Provider::"gh")
when { context has via && context.via == "queue-gateway"
    && context has agent && context.agent == "queue-writer" };

@id("enqueue-org-source")
permit(principal == Dekopon::Principal::"queue-writer",
       action == Dekopon::Action::"secret.use",
       resource == Dekopon::Secret::"drn:secret:github/org")
when { context.capability == "gh.pull-request.enqueue" && context.provider == "gh"
    && context.sink == "httpBearer" && context has via && context.via == "queue-gateway"
    && context has agent && context.agent == "queue-writer" };

@id("enqueue-http-fence")
forbid(principal, action == Dekopon::Action::"gh.pull-request.enqueue", resource)
when { context has request }
unless { context.request.url.host == "api.github.com"
    && ((context.request.method == "GET"
         && context.request.url.path like "/repos/dekopon-agents/*/pulls/*")
        || (context.request.method == "POST" && context.request.url.path == "/graphql")) };
