import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { useTranslation } from "react-i18next";

interface Props {
  /** The skill being forked; the dialog is closed when null. */
  skillName: string | null;
  existingNames: string[];
  onClose: () => void;
  /** Rejects to keep the dialog open; the caller reports the error. */
  onFork: (newName: string) => Promise<void>;
}

/** Asks for the new skill's name for "Fork" (sm-69q.7). */
export function ForkSkillDialog({ skillName, existingNames, onClose, onFork }: Props) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (skillName) setName(`${skillName}-fork`);
  }, [skillName]);

  if (!skillName) return null;

  const trimmed = name.trim();
  const taken = existingNames.some((n) => n.toLowerCase() === trimmed.toLowerCase());
  const canFork = trimmed.length > 0 && !taken && !/[\\/]/.test(trimmed);

  const handleFork = async () => {
    if (!canFork || loading) return;
    setLoading(true);
    try {
      await onFork(trimmed);
      onClose();
    } catch {
      // Reported by the caller; stay open so the name can be changed.
    } finally {
      setLoading(false);
    }
  };

  const inputClass =
    "w-full bg-background border border-border-subtle rounded-lg px-3 py-2 text-[13px] text-secondary focus:outline-none focus:border-border transition-all placeholder-faint";

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 z-0 bg-black/70 backdrop-blur-sm" onClick={onClose} />
      <div
        role="dialog"
        aria-modal="true"
        className="relative z-10 bg-surface border border-border rounded-xl w-full max-w-[420px] p-5 shadow-2xl"
      >
        <div className="flex items-center justify-between mb-2">
          <h2 className="text-[13px] font-semibold text-primary">
            {t("fork.title", { name: skillName })}
          </h2>
          <button
            onClick={onClose}
            aria-label={t("common.cancel")}
            className="text-muted hover:text-secondary p-1 rounded-md transition-colors outline-none"
          >
            <X className="w-4 h-4" />
          </button>
        </div>
        <p className="mb-4 text-[12px] text-muted">{t("fork.description")}</p>

        <div className="space-y-3">
          <div>
            <label className="block text-[13px] font-medium text-tertiary mb-1">
              {t("fork.nameLabel")}
            </label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className={inputClass}
              autoFocus
              onKeyDown={(e) => e.key === "Enter" && handleFork()}
            />
            {taken && (
              <p className="mt-1 text-[12px] text-amber-600 dark:text-amber-400">
                {t("fork.nameTaken")}
              </p>
            )}
          </div>
          <div className="flex justify-end gap-2 pt-1">
            <button
              onClick={onClose}
              className="px-3 py-1.5 rounded-lg text-[13px] font-medium text-tertiary hover:text-secondary hover:bg-surface-hover transition-colors outline-none"
            >
              {t("common.cancel")}
            </button>
            <button
              onClick={handleFork}
              disabled={!canFork || loading}
              className="px-3 py-1.5 rounded-lg bg-accent-dark hover:bg-accent text-white text-[13px] font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed border border-accent-border outline-none"
            >
              {loading ? t("common.loading") : t("fork.confirm")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
