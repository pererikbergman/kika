# Kika

A terminal git client that also shows and reviews pull requests and merge requests from the hosting service a repository lives on.

## Language

### Forges

**Forge**:
A hosting service for git repositories that Kika can ask about pull or merge requests, such as GitHub or GitLab.
_Avoid_: Provider, git provider, host service

**Forge kind**:
Which forge a host runs (GitHub, GitLab, …). Each forge kind has one **Forge adapter**.
_Avoid_: Provider type

**Forge adapter**:
The one piece of Kika that talks to one forge kind through that forge's own CLI; the rest of Kika sees only the shared **Forge** interface.
_Avoid_: Client, driver, backend

**Forge host**:
A host name the user has listed in the global config as being of a **forge kind**, such as `github.example.com`.
_Avoid_: Server, instance

## Example dialogue

> **Dev:** "When a repo's origin is on `gitlab.example.com`, which forge adapter runs?"
> **Owner:** "The GitLab one — but only if that forge host is opted in to being asked. Listing it just for links never starts the adapter."
