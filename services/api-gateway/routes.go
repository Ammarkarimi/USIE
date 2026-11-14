package api

// routes is the complete API surface. openapi.yaml must document every entry.
func routes() []route {
	const v = "/api/v1"
	return []route{
		{Method: "GET", Path: v + "/system", Handler: (*Server).getSystem},
		{Method: "GET", Path: v + "/openapi.yaml", Handler: (*Server).getOpenAPI},

		{Method: "POST", Path: v + "/auth/register", Handler: (*Server).register, AuthLimited: true},
		{Method: "POST", Path: v + "/auth/login", Handler: (*Server).login, AuthLimited: true},
		{Method: "POST", Path: v + "/auth/logout", MinRole: "viewer", Handler: (*Server).logout},
		{Method: "GET", Path: v + "/auth/me", MinRole: "viewer", Handler: (*Server).me},

		{Method: "GET", Path: v + "/tokens", MinRole: "viewer", Handler: (*Server).listTokens},
		{Method: "POST", Path: v + "/tokens", MinRole: "viewer", Handler: (*Server).createToken},
		{Method: "DELETE", Path: v + "/tokens/{id}", MinRole: "viewer", Handler: (*Server).revokeToken},

		{Method: "GET", Path: v + "/workspaces", MinRole: "viewer", Handler: (*Server).listWorkspaces},
		{Method: "POST", Path: v + "/workspaces", MinRole: "admin", Handler: (*Server).createWorkspace},

		{Method: "GET", Path: v + "/repositories", MinRole: "viewer", Handler: (*Server).listRepositories},
		{Method: "POST", Path: v + "/repositories", MinRole: "member", Handler: (*Server).createRepository},
		{Method: "GET", Path: v + "/repositories/{id}", MinRole: "viewer", Handler: (*Server).getRepository},
		{Method: "DELETE", Path: v + "/repositories/{id}", MinRole: "admin", Handler: (*Server).deleteRepository},
		{Method: "POST", Path: v + "/repositories/{id}/analyze", MinRole: "member", Handler: (*Server).analyzeRepository},
		{Method: "POST", Path: v + "/repositories/{id}/upload", MinRole: "member", Handler: (*Server).uploadRepository, Raw: true},
		{Method: "GET", Path: v + "/repositories/{id}/versions", MinRole: "viewer", Handler: (*Server).listVersions},
		{Method: "GET", Path: v + "/repositories/{id}/commits", MinRole: "viewer", Handler: (*Server).listCommits},
		{Method: "GET", Path: v + "/repositories/{id}/branches", MinRole: "viewer", Handler: (*Server).listBranches},

		{Method: "GET", Path: v + "/versions/{id}", MinRole: "viewer", Handler: (*Server).getVersion},
		{Method: "GET", Path: v + "/versions/{id}/tree", MinRole: "viewer", Handler: (*Server).getTree},
		{Method: "GET", Path: v + "/versions/{id}/files", MinRole: "viewer", Handler: (*Server).listFiles},
		{Method: "GET", Path: v + "/versions/{id}/file", MinRole: "viewer", Handler: (*Server).getFile},
		{Method: "GET", Path: v + "/versions/{id}/dependencies", MinRole: "viewer", Handler: (*Server).listDependencies},
		{Method: "GET", Path: v + "/versions/{id}/references", MinRole: "viewer", Handler: (*Server).listReferences},
		{Method: "GET", Path: v + "/versions/{id}/graph/stats", MinRole: "viewer", Handler: (*Server).graphStats},
		{Method: "GET", Path: v + "/versions/{id}/graph/search", MinRole: "viewer", Handler: (*Server).graphSearch},
		{Method: "GET", Path: v + "/versions/{id}/graph/node", MinRole: "viewer", Handler: (*Server).graphNode},
		{Method: "GET", Path: v + "/versions/{id}/graph/neighborhood", MinRole: "viewer", Handler: (*Server).graphNeighborhood},
		{Method: "GET", Path: v + "/versions/{id}/graph/files", MinRole: "viewer", Handler: (*Server).graphFiles},
		{Method: "GET", Path: v + "/versions/{id}/graph/diff", MinRole: "viewer", Handler: (*Server).graphDiff},

		{Method: "GET", Path: v + "/jobs", MinRole: "viewer", Handler: (*Server).listJobs},
		{Method: "GET", Path: v + "/jobs/{id}", MinRole: "viewer", Handler: (*Server).getJob},
		{Method: "POST", Path: v + "/jobs/{id}/cancel", MinRole: "member", Handler: (*Server).cancelJob},
		{Method: "GET", Path: v + "/jobs/{id}/events", MinRole: "viewer", Handler: (*Server).jobEvents, Raw: true},

		{Method: "GET", Path: v + "/workers", MinRole: "viewer", Handler: (*Server).listWorkers},
		{Method: "GET", Path: v + "/audit-events", MinRole: "admin", Handler: (*Server).listAudit},
	}
}
