import { Button, Disclosure } from "@heroui/react";
import type { ReactNode } from "react";
import { Component } from "react";
// A class component can't use the useTranslation() hook — this app's shared
// i18next instance (initialized as a side effect of importing "../../lib/i18n"
// in App.tsx before this ever renders) is called directly instead.
//
// Every t() passes a literal defaultValue. This is the component that renders
// when something ELSE broke, and i18n init is one of the things that can break:
// an uninitialized i18next returns the KEY, so the user would read
// "errorBoundaryTitle" instead of a sentence. The English literal is what was
// hard-coded here before the strings were localized.
import i18n from "i18next";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "./empty";

interface Props {
	children: ReactNode;
	fallback?: ReactNode;
}

interface State {
	error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
	state: State = { error: null };

	static getDerivedStateFromError(error: Error) {
		return { error };
	}

	render() {
		if (this.state.error) {
			return (
				this.props.fallback ?? (
					<div
						className="flex h-full items-center justify-center p-6"
						role="alert"
						aria-live="polite"
					>
						<Empty>
							<EmptyHeader>
								<EmptyTitle>
									{i18n.t(
										"errorBoundaryTitle",
										"Something went wrong",
									)}
								</EmptyTitle>
								<EmptyDescription>
									{i18n.t(
										"errorBoundaryDescription",
										"This part of the app couldn't load.",
									)}
								</EmptyDescription>
							</EmptyHeader>
							<Button
								variant="outline"
								size="sm"
								onPress={() => this.setState({ error: null })}
							>
								{i18n.t("retry", "Try again")}
							</Button>
							<Disclosure className="mt-2 w-full max-w-md text-left">
								<Disclosure.Trigger className="text-xs text-muted">
									{i18n.t(
										"errorBoundaryDetailsToggle",
										"Show technical details",
									)}
								</Disclosure.Trigger>
								<Disclosure.Content>
									<pre className="mt-1 overflow-x-auto text-xs break-words whitespace-pre-wrap text-muted">
										{this.state.error.message}
									</pre>
								</Disclosure.Content>
							</Disclosure>
						</Empty>
					</div>
				)
			);
		}
		return this.props.children;
	}
}
