import {
	ArrowPathIcon,
	CheckCircleIcon,
	XCircleIcon,
} from "@heroicons/react/24/solid";

interface ResultStatusItemProps {
	displayName: string;
	status: "pending" | "success" | "error";
	statusText: string;
	error?: string;
}

export function ResultStatusItem({
	displayName,
	status,
	statusText,
	error,
}: ResultStatusItemProps) {
	return (
		<div className="flex items-start gap-2 rounded-lg bg-surface-secondary p-3">
			{status === "pending" && (
				<ArrowPathIcon className="mt-0.5 size-4 shrink-0 animate-spin text-muted" />
			)}
			{status === "success" && (
				<CheckCircleIcon className="mt-0.5 size-4 shrink-0 text-success" />
			)}
			{status === "error" && (
				<XCircleIcon className="mt-0.5 size-4 shrink-0 text-danger" />
			)}
			<div className="min-w-0">
				<p className="text-sm font-medium">{displayName}</p>
				<p
					className="text-xs text-muted"
					role={error ? "alert" : undefined}
					aria-live={error ? "assertive" : undefined}
				>
					{error || statusText}
				</p>
			</div>
		</div>
	);
}
