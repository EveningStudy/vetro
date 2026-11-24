import { useEffect, useState } from "react";
import { useT } from "~/i18n";
import { actions } from "~/state/store";
import { Button } from "./ui/button";
import { Dialog, DialogDescription, DialogPopup, DialogTitle } from "./ui/overlay";
import { Input } from "./ui/primitives";

export function RenameDialog({
  open,
  onOpenChange,
  threadId,
  title,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  threadId: string;
  title: string;
}) {
  const t = useT();
  const [value, setValue] = useState(title);
  useEffect(() => {
    if (open) setValue(title);
  }, [open, title]);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup className="max-w-md">
        <form
          className="flex flex-col gap-4 p-5"
          onSubmit={(event) => {
            event.preventDefault();
            if (value.trim() && value.trim() !== title) void actions.updateThread(threadId, { title: value.trim() });
            onOpenChange(false);
          }}
        >
          <div className="flex flex-col gap-1 pe-8">
            <DialogTitle>{t("thread.renameTitle")}</DialogTitle>
            <DialogDescription>{t("thread.renameHint")}</DialogDescription>
          </div>
          <Input autoFocus value={value} onChange={(event) => setValue(event.target.value)} />
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => onOpenChange(false)}>
              {t("common.cancel")}
            </Button>
            <Button type="submit">{t("common.save")}</Button>
          </div>
        </form>
      </DialogPopup>
    </Dialog>
  );
}
