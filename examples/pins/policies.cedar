@id("pin-agent")
permit(principal == Dekopon::Principal::"pin-writer",
       action == Dekopon::Action::"agent.prompt",
       resource == Dekopon::Agent::"pin-writer")
when { context has via && context.via == "pin-gateway" };

@id("pin-update")
permit(principal == Dekopon::Principal::"pin-writer",
       action == Dekopon::Action::"gh.pin.update",
       resource == Dekopon::Provider::"gh")
when { context has via && context.via == "pin-gateway"
    && context has agent && context.agent == "pin-writer" };

@id("pin-merge")
permit(principal == Dekopon::Principal::"pin-writer",
       action == Dekopon::Action::"gh.pin.merge",
       resource == Dekopon::Provider::"gh")
when { context has via && context.via == "pin-gateway"
    && context has agent && context.agent == "pin-writer" };

@id("pin-xrl-source")
permit(principal == Dekopon::Principal::"pin-writer",
       action == Dekopon::Action::"secret.use",
       resource == Dekopon::Secret::"drn:secret:github/xrl")
when { (context.capability == "gh.pin.update" || context.capability == "gh.pin.merge")
    && context.provider == "gh" && context.sink == "httpBearer"
    && context has via && context.via == "pin-gateway"
    && context has agent && context.agent == "pin-writer" };

@id("pin-update-http-fence")
forbid(principal, action == Dekopon::Action::"gh.pin.update", resource)
when { context has request }
unless { context.request.url.host == "api.github.com"
    && ((context.request.method == "GET"
         && (context.request.url.path == "/repos/xrl/rpi-homelab/git/ref/heads/main"
             || context.request.url.path like "/repos/xrl/rpi-homelab/contents/*"))
        || (context.request.method == "POST"
            && (context.request.url.path == "/repos/xrl/rpi-homelab/git/refs"
                || context.request.url.path == "/repos/xrl/rpi-homelab/pulls"))
        || (context.request.method == "PUT"
            && context.request.url.path like "/repos/xrl/rpi-homelab/contents/*")) };

@id("pin-merge-http-fence")
forbid(principal, action == Dekopon::Action::"gh.pin.merge", resource)
when { context has request }
unless { context.request.url.host == "api.github.com"
    && ((context.request.method == "GET"
         && (context.request.url.path like "/repos/xrl/rpi-homelab/pulls/*"
             || context.request.url.path like "/repos/xrl/rpi-homelab/compare/*"
             || context.request.url.path like "/repos/xrl/rpi-homelab/git/commits/*"
             || context.request.url.path like "/repos/xrl/rpi-homelab/git/trees/*"
             || context.request.url.path like "/repos/xrl/rpi-homelab/contents/*"))
        || (context.request.method == "PUT"
            && context.request.url.path like "/repos/xrl/rpi-homelab/pulls/*/merge")) };
