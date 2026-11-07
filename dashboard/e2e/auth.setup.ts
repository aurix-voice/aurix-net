import { expect, test as setup } from "@playwright/test";

import { OPERATOR_STATE, operatorToken, pinEnglish, signIn } from "./env";

setup("operator signs in with the admin token", async ({ page }) => {
  await pinEnglish(page);
  await signIn(page, operatorToken());
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
  await expect(page.getByText("Ready")).toBeVisible();
  await page.context().storageState({ path: OPERATOR_STATE });
});
