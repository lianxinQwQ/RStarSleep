"""Source identities for incremental makepkg builds; no PKGBUILD execution here."""
import re
from pathlib import Path
from urllib.parse import urlsplit

REVISION = re.compile(r"[0-9a-f]{40}|[0-9a-f]{64}")


def source_specs(sources, unchecked=()):
    """Return supported Git sources and a reason when reuse cannot be verified."""
    result, aliases = [], set()
    for source in sources:
        alias, separator, url = source.partition("::")
        if not separator:
            url, alias = source, ""
        protocol = url.split(":", 1)[0]
        if protocol.startswith(("svn", "hg", "bzr", "fossil")):
            return [], "unsupported VCS source"
        if protocol != "git" and not protocol.startswith("git+"):
            if "://" in url and source in unchecked:
                return [], "remote source without a fixed checksum"
            continue
        url = url.removeprefix("git+")
        # makepkg accepts ?signed following the reference, not a general URL query.
        if url.endswith("?signed"):
            url = url[:-7]
        remote, separator, fragment = url.partition("#")
        parsed = urlsplit(remote)
        if parsed.query or parsed.scheme not in ("git", "https", "http", "ssh", "file"):
            return [], "unsupported Git URL"
        kind, ref = "head", "HEAD"
        if separator:
            kind, equals, ref = fragment.partition("=")
            if not equals or kind not in ("branch", "tag", "commit") or not ref:
                return [], "unsupported Git reference"
        if kind == "commit" and not REVISION.fullmatch(ref):
            return [], "Git commit must be a full object ID"
        if any(c.isspace() for c in ref) or ref.startswith("-"):
            return [], "invalid Git reference"
        if not alias:
            alias = Path(parsed.path.rstrip("/")).name.split(".git", 1)[0]
        if not alias or alias in (".", "..") or "/" in alias or "\\" in alias or alias in aliases:
            return [], "ambiguous Git source directory"
        aliases.add(alias)
        result.append(dict(source=source, url=remote, alias=alias, kind=kind, ref=ref))
    return result, None


def remote_revisions(user, specs):
    """Resolve exactly the tracked refs, including peeled annotated tags."""
    revisions = {}
    for source in specs:
        if source["kind"] == "commit":
            revisions[source["source"]] = source["ref"]
            continue
        reference = {"head": "HEAD", "branch": "refs/heads/" + source["ref"],
                     "tag": "refs/tags/" + source["ref"]}[source["kind"]]
        refs = [reference, reference + "^{}"] if source["kind"] == "tag" else [reference]
        output = user(["git", "ls-remote", "--symref", "--", source["url"], *refs], capture=True).stdout
        found = {}
        for line in output.splitlines():
            fields = line.split()
            if source["kind"] == "head" and len(fields) == 3 and fields[0] == "ref:" and fields[2] == "HEAD":
                source["head_ref"] = fields[1]
            if len(fields) == 2 and fields[1] in refs and REVISION.fullmatch(fields[0]):
                found[fields[1]] = fields[0]
        revision = found.get(reference + "^{}", found.get(reference))
        if not revision:
            raise ValueError(f"Git reference unavailable: {source['url']}#{source['kind']}={source['ref']}")
        revisions[source["source"]] = revision
    return revisions


def actual_revisions(user, specs, source_directory, recipe_directory=None):
    """Read downloaded refs consumed by --holdver, before prepare() can change HEAD."""
    revisions = {}
    for source in specs:
        checkout = Path(source_directory) / source["alias"]
        if recipe_directory is not None and (Path(recipe_directory) / source["alias"]).is_dir():
            checkout = Path(recipe_directory) / source["alias"]
        if not checkout.is_dir():
            raise ValueError(f"downloaded Git source unavailable: {checkout}")
        command = ["git", "-c", "safe.bareRepository=all", "-C", str(checkout)]
        if source.get("head_ref"):
            # git fetch does not update a mirror's HEAD on default branch changes.
            user([*command, "symbolic-ref", "HEAD", source["head_ref"]])
        ref = {"head": "HEAD", "branch": "refs/heads/" + source["ref"],
               "tag": "refs/tags/" + source["ref"], "commit": source["ref"]}[source["kind"]]
        revision = user([*command, "rev-parse", "--verify", ref + "^{commit}"],
                        capture=True).stdout.strip()
        if not REVISION.fullmatch(revision):
            raise ValueError(f"invalid Git source revision: {checkout}")
        revisions[source["source"]] = revision
    return revisions
