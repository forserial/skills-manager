import { useEffect, useState } from "react";
import { GitCommitHorizontal, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { getErrorMessage } from "../lib/error";
import { getSkillUpstreamCommits, type UpstreamCommits } from "../lib/pinningApi";

/** The upstream commits since the skill's baseline that touch its directory,
 *  shown above the source diff (sm-69q.5). Loads when mounted. */
export function SkillUpstreamCommits({ skillId }: { skillId: string }) {
  const { t } = useTranslation();
  const [result, setResult] = useState<UpstreamCommits | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getSkillUpstreamCommits(skillId)
      .then((value) => {
        if (!cancelled) setResult(value);
      })
      .catch((err) => {
        if (!cancelled) setError(getErrorMessage(err, t("common.error")));
      });
    return () => {
      cancelled = true;
    };
  }, [skillId, t]);

  const header = (
    <div className="flex items-center gap-2 px-4 py-3">
      <GitCommitHorizontal className="h-3.5 w-3.5 text-muted" />
      <span className="text-[13px] font-medium text-secondary">{t("pin.commits.title")}</span>
      {result && result.commits.length > 0 && (
        <span className="text-[12px] text-muted">
          {t("pin.commits.count", { count: result.commits.length })}
          {result.truncated ? "+" : ""}
        </span>
      )}
    </div>
  );

  let body;
  if (error) {
    body = <p className="px-4 py-2.5 text-[12px] text-muted">{t("pin.commits.failed", { error })}</p>;
  } else if (!result) {
    body = (
      <p className="flex items-center gap-2 px-4 py-2.5 text-[12px] text-muted">
        <Loader2 className="h-3.5 w-3.5 animate-spin" />
        {t("common.loading")}
      </p>
    );
  } else if (!result.reachable) {
    body = <p className="px-4 py-2.5 text-[12px] text-muted">{result.message}</p>;
  } else if (result.commits.length === 0) {
    body = <p className="px-4 py-2.5 text-[12px] text-muted">{t("pin.commits.none")}</p>;
  } else {
    body = (
      <ul className="divide-y divide-border-subtle">
        {result.commits.map((commit) => (
          <li key={commit.sha} className="flex min-w-0 items-baseline gap-2 px-4 py-2">
            <span className="shrink-0 font-mono text-[12px] text-muted" title={commit.sha}>
              {commit.sha.slice(0, 7)}
            </span>
            <span className="min-w-0 flex-1 truncate text-[13px] text-secondary" title={commit.subject}>
              {commit.subject}
            </span>
            <span className="shrink-0 text-[12px] text-faint">
              {commit.author} · {new Date(commit.date).toLocaleDateString()}
            </span>
          </li>
        ))}
      </ul>
    );
  }

  return (
    <section className="mb-4 rounded-xl border border-border-subtle bg-surface/70">
      {header}
      <div className="border-t border-border-subtle">{body}</div>
    </section>
  );
}
