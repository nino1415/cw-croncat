use crate::error::ContractError;
use crate::helpers::{send_tokens, GenericBalance};
use crate::state::{Config, CwCroncat};
use cosmwasm_std::{
    has_coins, Addr, Coin, Deps, DepsMut, Env, MessageInfo, Response, StdResult, Storage, SubMsg,
};
use cw20::Balance;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub enum AgentStatus {
    // Default for any new agent, if tasks ratio allows
    Active,

    // Default for any new agent, until more tasks come online
    Pending,

    // More tasks are available, agent must checkin to become active
    Nominated,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct Agent {
    pub status: AgentStatus,

    // Where rewards get transferred
    pub payable_account_id: Addr,

    // accrued reward balance
    pub balance: GenericBalance,

    // stats
    pub total_tasks_executed: u64,

    // Holds slot number of a missed slot.
    // If other agents see an agent miss a slot, they store the missed slot number.
    // If agent does a task later, this number is reset to zero.
    // Example data: 1633890060000000000 or 0
    pub last_missed_slot: u64,

    // Timestamp of when agent first registered
    // Useful for rewarding agents for their patience while they are pending and operating service
    // Agent will be responsible to constantly monitor when it is their turn to join in active agent set (done as part of agent code loops)
    // Example data: 1633890060000000000 or 0
    pub register_start: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct GetAgentIdsResponse {
    active: Vec<Addr>,
    pending: Vec<Addr>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
pub struct GetAgentTasksResponse(u64, u128);

impl<'a> CwCroncat<'a> {
    /// Get a single agent details
    /// Check's status as well, in case this agent needs to be considered for election
    pub(crate) fn query_get_agent(&self, deps: Deps, account_id: Addr) -> StdResult<Option<Agent>> {
        let agent = self.agents.may_load(deps.storage, account_id.clone())?;
        if agent.is_none() {
            return Ok(None);
        }
        let a = agent.unwrap();

        let pending: Vec<Addr> = self
            .agent_pending_queue
            .may_load(deps.storage)?
            .unwrap_or_default();

        // If agent is pending, Check if they should get nominated to checkin to become active
        let agent_status: AgentStatus = if a.status == AgentStatus::Pending {
            // TODO: change to check total tasks + task ratio
            if pending.contains(&account_id) {
                AgentStatus::Nominated
            } else {
                a.status
            }
        } else {
            a.status
        };

        Ok(Some(Agent {
            status: agent_status,
            payable_account_id: a.payable_account_id,
            balance: a.balance,
            total_tasks_executed: a.total_tasks_executed,
            last_missed_slot: a.last_missed_slot,
            register_start: a.register_start,
        }))
    }

    /// Get a list of agent addresses
    pub(crate) fn query_get_agent_ids(&self, deps: Deps) -> StdResult<GetAgentIdsResponse> {
        // let active = self.agent_active_queue.load(deps.storage)?;
        // let pending = self.agent_pending_queue.load(deps.storage)?;
        let active: Vec<Addr> = self
            .agent_active_queue
            .may_load(deps.storage)?
            .unwrap_or_default();
        let pending: Vec<Addr> = self
            .agent_pending_queue
            .may_load(deps.storage)?
            .unwrap_or_default();

        Ok(GetAgentIdsResponse { active, pending })
        // Ok(GetAgentIdsResponse(active, pending))
    }

    // TODO:
    /// Check how many tasks an agent can execute
    pub(crate) fn query_get_agent_tasks(
        &self,
        _deps: Deps,
        _account_id: Addr,
    ) -> StdResult<GetAgentTasksResponse> {
        // let active = self.agent_active_queue.load(deps.storage)?;

        Ok(GetAgentTasksResponse(0, 0))
    }

    /// Add any account as an agent that will be able to execute tasks.
    /// Registering allows for rewards accruing with micro-payments which will accumulate to more long-term.
    ///
    /// Optional Parameters:
    /// "payable_account_id" - Allows a different account id to be specified, so a user can receive funds at a different account than the agent account.
    pub fn register_agent(
        &self,
        deps: DepsMut,
        info: MessageInfo,
        env: Env,
        payable_account_id: Option<Addr>,
    ) -> Result<Response, ContractError> {
        if !info.funds.is_empty() {
            return Err(ContractError::CustomError {
                val: "Do not attach funds".to_string(),
            });
        }
        let c: Config = self.config.load(deps.storage)?;
        if c.paused {
            return Err(ContractError::CustomError {
                val: "Register agent paused".to_string(),
            });
        }

        let account = info.sender;

        // REF: https://github.com/CosmWasm/cw-tokens/tree/main/contracts/cw20-escrow
        // Check if native token balance is sufficient for a few txns, in this case 4 txns
        // TODO: Adjust gas & costs based on real usage cost
        let agent_wallet_balances = deps.querier.query_all_balances(account.clone())?;
        let unit_cost = c.gas_price * 4;
        if !has_coins(
            &agent_wallet_balances,
            &Coin::new(u128::from(unit_cost), c.native_denom),
        ) || agent_wallet_balances.is_empty()
        {
            return Err(ContractError::CustomError {
                val: "Insufficient funds".to_string(),
            });
        }

        let payable_id = payable_account_id.unwrap_or_else(|| account.clone());

        let mut active_agents: Vec<Addr> = self
            .agent_active_queue
            .may_load(deps.storage)?
            .unwrap_or_default();
        let total_agents = active_agents.len();
        let agent_status = if total_agents == 0 {
            active_agents.push(account.clone());
            self.agent_active_queue.save(deps.storage, &active_agents)?;
            AgentStatus::Active
        } else {
            let mut pending_agents = self
                .agent_pending_queue
                .may_load(deps.storage)?
                .unwrap_or_default();
            pending_agents.push(account.clone());
            self.agent_pending_queue
                .save(deps.storage, &pending_agents)?;
            AgentStatus::Pending
        };

        self.agents.update(
            deps.storage,
            account,
            |a: Option<Agent>| -> Result<_, ContractError> {
                match a {
                    // make sure that account isn't already added
                    Some(_) => Err(ContractError::CustomError {
                        val: "Agent already exists".to_string(),
                    }),
                    None => {
                        Ok(Agent {
                            status: agent_status.clone(),
                            payable_account_id: payable_id,
                            balance: GenericBalance::default(),
                            total_tasks_executed: 0,
                            last_missed_slot: 0,
                            // REF: https://github.com/CosmWasm/cosmwasm/blob/main/packages/std/src/types.rs#L57
                            register_start: env.block.time.nanos(),
                        })
                    }
                }
            },
        )?;

        Ok(Response::new()
            .add_attribute("method", "register_agent")
            .add_attribute("agent_status", format!("{:?}", agent_status))
            .add_attribute("register_start", env.block.time.nanos().to_string()))
    }

    /// Update agent details, specifically the payable account id for an agent.
    pub fn update_agent(
        &self,
        deps: DepsMut,
        info: MessageInfo,
        _env: Env,
        payable_account_id: Addr,
    ) -> Result<Response, ContractError> {
        let c: Config = self.config.load(deps.storage)?;
        if c.paused {
            return Err(ContractError::CustomError {
                val: "Register agent paused".to_string(),
            });
        }

        self.agents.update(
            deps.storage,
            info.sender,
            |a: Option<Agent>| -> Result<_, ContractError> {
                match a {
                    Some(agent) => {
                        let mut ag = agent;
                        ag.payable_account_id = payable_account_id;
                        Ok(ag)
                    }
                    None => Err(ContractError::CustomError {
                        val: "Agent doesnt exist".to_string(),
                    }),
                }
            },
        )?;

        Ok(Response::new().add_attribute("method", "update_agent"))
    }

    /// Allows an agent to withdraw all rewards, paid to the specified payable account id.
    pub(crate) fn withdraw_balances(
        &self,
        storage: &mut dyn Storage,
        info: MessageInfo,
    ) -> Result<Vec<SubMsg>, ContractError> {
        let a = self.agents.may_load(storage, info.sender)?;
        if a.is_none() {
            return Err(ContractError::CustomError {
                val: "Agent doesnt exist".to_string(),
            });
        }
        let agent = a.unwrap();

        // This will send all token balances to Agent
        let (messages, balances) = send_tokens(&agent.payable_account_id, &agent.balance)?;
        println!("balancesbalances {:?}", balances);
        let mut config = self.config.load(storage)?;
        config
            .available_balance
            .minus_tokens(Balance::from(balances.native));
        // TODO: Finish:
        // config
        //     .available_balance
        //     .minus_tokens(Balance::from(balances.cw20));
        self.config.save(storage, &config)?;

        Ok(messages)
    }

    /// Allows an agent to withdraw all rewards, paid to the specified payable account id.
    pub fn withdraw_task_balance(
        &self,
        deps: DepsMut,
        info: MessageInfo,
        _env: Env,
    ) -> Result<Response, ContractError> {
        let messages = self.withdraw_balances(deps.storage, info.clone())?;

        Ok(Response::new()
            .add_attribute("method", "withdraw_task_balance")
            .add_attribute("account_id", info.sender)
            .add_submessages(messages))
    }

    /// Allows an agent to accept a nomination within a certain amount of time to become an active agent.
    pub fn accept_nomination_agent(
        &self,
        _deps: DepsMut,
        _info: MessageInfo,
        _env: Env,
    ) -> Result<Response, ContractError> {
        Ok(Response::new().add_attribute("method", "accept_nomination_agent"))
    }

    /// Removes the agent from the active set of agents.
    /// Withdraws all reward balances to the agent payable account id.
    pub fn unregister_agent(
        &self,
        deps: DepsMut,
        info: MessageInfo,
        _env: Env,
    ) -> Result<Response, ContractError> {
        // Get withdraw messages, if any
        // NOTE: Since this also checks if agent exists, safe to not have redundant logic
        let messages = self.withdraw_balances(deps.storage, info.clone())?;
        let agent_id = info.sender;
        self.agents.remove(deps.storage, agent_id.clone());

        let responses = Response::new()
            .add_attribute("method", "unregister_agent")
            .add_attribute("account_id", agent_id);

        if messages.is_empty() {
            Ok(responses)
        } else {
            Ok(responses.add_submessages(messages))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ContractError;
    use crate::helpers::CwTemplateContract;
    use crate::msg::{ExecuteMsg, InstantiateMsg, QueryMsg};
    use cosmwasm_std::{coin, coins, CosmosMsg, Addr, Empty, Timestamp, WasmMsg, to_binary};
    use cw_multi_test::{App, AppBuilder, Contract, ContractWrapper, Executor};
    use crate::slots::{Boundary, Interval};
    use crate::tasks::TaskRequest;

    pub fn contract_template() -> Box<dyn Contract<Empty>> {
        let contract = ContractWrapper::new(
            crate::entry::execute,
            crate::entry::instantiate,
            crate::entry::query,
        );
        Box::new(contract)
    }

    const AGENT0: &str = "AGENT000";
    const AGENT1: &str = "AGENT001";
    const AGENT2: &str = "AGENT002";
    const PARTICIPANT: &str = "PARTICIPANT";
    const AGENT1_BENEFICIARY: &str = "AGENT001_BENEFICIARY";
    const ADMIN: &str = "ADMIN";
    const NATIVE_DENOM: &str = "atom";

    fn mock_app() -> App {
        AppBuilder::new().build(|router, _, storage| {
            let accounts: Vec<(u128, String)> = vec![
                (100, ADMIN.to_string()),
                (1, AGENT0.to_string()),
                (100, AGENT1.to_string()),
                (100, AGENT2.to_string()),
                (1, AGENT1_BENEFICIARY.to_string()),
                (999, PARTICIPANT.to_string()),
            ];
            for (amt, address) in accounts.iter() {
                router
                    .bank
                    .init_balance(
                        storage,
                        &Addr::unchecked(address),
                        vec![coin(amt.clone(), NATIVE_DENOM.to_string())],
                    )
                    .unwrap();
            }
        })
    }

    fn proper_instantiate() -> (App, CwTemplateContract) {
        let mut app = mock_app();
        let cw_template_id = app.store_code(contract_template());
        let owner_addr = Addr::unchecked(ADMIN);

        let msg = InstantiateMsg {
            denom: "atom".to_string(),
            owner_id: Some(owner_addr.clone()),
        };
        let cw_template_contract_addr = app
            .instantiate_contract(cw_template_id, owner_addr, &msg, &[], "Manager", None)
            .unwrap();

        let cw_template_contract = CwTemplateContract(cw_template_contract_addr);

        (app, cw_template_contract)
    }

    #[test]
    fn register_agent_fail_cases() {
        let (mut app, cw_template_contract) = proper_instantiate();
        let contract_addr = cw_template_contract.addr();

        // start first register
        let msg = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };

        // Test funds fail register if sent
        let rereg_err = app
            .execute_contract(
                Addr::unchecked(AGENT1),
                contract_addr.clone(),
                &msg,
                &coins(37, "atom"),
            )
            .unwrap_err();
        println!("rereg_errrereg_err {:?}", rereg_err);
        assert_eq!(
            ContractError::CustomError {
                val: "Do not attach funds".to_string()
            },
            rereg_err.downcast().unwrap()
        );

        // Test Can't register if contract is paused
        let payload_1 = ExecuteMsg::UpdateSettings {
            paused: Some(true),
            owner_id: None,
            // treasury_id: None,
            agent_fee: None,
            agent_task_ratio: None,
            agents_eject_threshold: None,
            gas_price: None,
            proxy_callback_gas: None,
            slot_granularity: None,
        };

        app.execute_contract(
            Addr::unchecked(ADMIN),
            contract_addr.clone(),
            &payload_1,
            &[],
        )
        .unwrap();
        let rereg_err = app
            .execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg, &[])
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Register agent paused".to_string()
            },
            rereg_err.downcast().unwrap()
        );

        // Test wallet rejected if doesnt have enough funds
        let payload_2 = ExecuteMsg::UpdateSettings {
            paused: Some(false),
            owner_id: None,
            // treasury_id: None,
            agent_fee: None,
            agent_task_ratio: None,
            agents_eject_threshold: None,
            gas_price: None,
            proxy_callback_gas: None,
            slot_granularity: None,
        };

        app.execute_contract(
            Addr::unchecked(ADMIN),
            contract_addr.clone(),
            &payload_2,
            &[],
        )
        .unwrap();
        let rereg_err = app
            .execute_contract(Addr::unchecked(AGENT0), contract_addr.clone(), &msg, &[])
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Insufficient funds".to_string()
            },
            rereg_err.downcast().unwrap()
        );
    }

    #[test]
    fn register_agent() {
        let (mut app, cw_template_contract) = proper_instantiate();
        let contract_addr = cw_template_contract.addr();
        let blk_time = app.block_info().time;

        // start first register
        let msg = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };
        app.execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg, &[])
            .unwrap();

        // check state to see if worked
        let value: GetAgentIdsResponse = app
            .wrap()
            .query_wasm_smart(&contract_addr.clone(), &QueryMsg::GetAgentIds {})
            .unwrap();
        assert_eq!(1, value.active.len());
        assert_eq!(0, value.pending.len());

        // message response matches expectations (same block, all the defaults)
        let agent_info: Agent = app
            .wrap()
            .query_wasm_smart(
                &contract_addr.clone(),
                &QueryMsg::GetAgent {
                    account_id: Addr::unchecked(AGENT1),
                },
            )
            .unwrap();
        println!("agent_infoagent_info {:?}", agent_info);
        assert_eq!(AgentStatus::Active, agent_info.status);
        assert_eq!(
            Addr::unchecked(AGENT1_BENEFICIARY),
            agent_info.payable_account_id
        );
        assert_eq!(GenericBalance::default(), agent_info.balance);
        assert_eq!(0, agent_info.total_tasks_executed);
        assert_eq!(0, agent_info.last_missed_slot);
        assert_eq!(blk_time, Timestamp::from_nanos(agent_info.register_start));

        // test fail if try to re-register
        let rereg_err = app
            .execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg, &[])
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Agent already exists".to_string()
            },
            rereg_err.downcast().unwrap()
        );

        // test another register, put into pending queue
        let msg2 = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };
        app.execute_contract(Addr::unchecked(AGENT2), contract_addr.clone(), &msg2, &[])
            .unwrap();

        // check state to see if worked
        let value: GetAgentIdsResponse = app
            .wrap()
            .query_wasm_smart(&contract_addr.clone(), &QueryMsg::GetAgentIds {})
            .unwrap();
        assert_eq!(1, value.active.len());
        assert_eq!(1, value.pending.len());
    }

    #[test]
    fn update_agent() {
        let (mut app, cw_template_contract) = proper_instantiate();
        let contract_addr = cw_template_contract.addr();

        // start first register
        let msg1 = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };
        app.execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg1, &[])
            .unwrap();

        // Fails for non-exist agents
        let msg = ExecuteMsg::UpdateAgent {
            payable_account_id: Addr::unchecked(AGENT0),
        };
        let update_err = app
            .execute_contract(Addr::unchecked(AGENT0), contract_addr.clone(), &msg, &[])
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Agent doesnt exist".to_string()
            },
            update_err.downcast().unwrap()
        );

        app.execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg, &[])
            .unwrap();

        // payable account was in fact updated
        let agent_info: Agent = app
            .wrap()
            .query_wasm_smart(
                &contract_addr.clone(),
                &QueryMsg::GetAgent {
                    account_id: Addr::unchecked(AGENT1),
                },
            )
            .unwrap();
        assert_eq!(Addr::unchecked(AGENT0), agent_info.payable_account_id);
    }

    #[test]
    fn unregister_agent() {
        let (mut app, cw_template_contract) = proper_instantiate();
        let contract_addr = cw_template_contract.addr();

        // start first register
        let msg1 = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };
        app.execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg1, &[])
            .unwrap();

        // Fails for non-exist agents
        let unreg_msg = ExecuteMsg::UnregisterAgent {};
        let update_err = app
            .execute_contract(
                Addr::unchecked(AGENT0),
                contract_addr.clone(),
                &unreg_msg,
                &[],
            )
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Agent doesnt exist".to_string()
            },
            update_err.downcast().unwrap()
        );

        // Get quick data about account before, to compare later
        let agent_bal = app
            .wrap()
            .query_balance(&Addr::unchecked(AGENT1), NATIVE_DENOM)
            .unwrap();
        assert_eq!(agent_bal, coin(100, NATIVE_DENOM));

        // Attempt the unregister
        app.execute_contract(
            Addr::unchecked(AGENT1),
            contract_addr.clone(),
            &unreg_msg,
            &[],
        )
        .unwrap();

        // Agent should not exist now
        let update_err = app
            .execute_contract(
                Addr::unchecked(AGENT1),
                contract_addr.clone(),
                &unreg_msg,
                &[],
            )
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Agent doesnt exist".to_string()
            },
            update_err.downcast().unwrap()
        );

        // Agent should have appropriate balance change
        // NOTE: Needs further checks when tasks can be performed
        let agent_bal = app
            .wrap()
            .query_balance(&Addr::unchecked(AGENT1), NATIVE_DENOM)
            .unwrap();
        assert_eq!(agent_bal, coin(100, NATIVE_DENOM));
    }

    #[test]
    fn withdraw_task_balance() {
        let (mut app, cw_template_contract) = proper_instantiate();
        let contract_addr = cw_template_contract.addr();

        // start first register
        let msg1 = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };
        app.execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg1, &[])
            .unwrap();

        // Fails for non-exist agents
        let wthdrw_msg = ExecuteMsg::WithdrawReward {};
        let update_err = app
            .execute_contract(
                Addr::unchecked(AGENT0),
                contract_addr.clone(),
                &wthdrw_msg,
                &[],
            )
            .unwrap_err();
        assert_eq!(
            ContractError::CustomError {
                val: "Agent doesnt exist".to_string()
            },
            update_err.downcast().unwrap()
        );

        // Get quick data about account before, to compare later
        let agent_bal = app
            .wrap()
            .query_balance(&Addr::unchecked(AGENT1), NATIVE_DENOM)
            .unwrap();
        assert_eq!(agent_bal, coin(100, NATIVE_DENOM));

        // Attempt the withdraw
        app.execute_contract(
            Addr::unchecked(AGENT1),
            contract_addr.clone(),
            &wthdrw_msg,
            &[],
        )
        .unwrap();

        // Agent should have appropriate balance change
        // NOTE: Needs further checks when tasks can be performed
        let agent_bal = app
            .wrap()
            .query_balance(&Addr::unchecked(AGENT1), NATIVE_DENOM)
            .unwrap();
        assert_eq!(agent_bal, coin(100, NATIVE_DENOM));
    }

    #[test]
    fn accept_nomination_agent() {
        let (mut app, cw_template_contract) = proper_instantiate();
        let contract_addr = cw_template_contract.addr();

        // Register agent
        let msg1 = ExecuteMsg::RegisterAgent {
            payable_account_id: Some(Addr::unchecked(AGENT1_BENEFICIARY)),
        };
        app.execute_contract(Addr::unchecked(AGENT1), contract_addr.clone(), &msg1, &[])
            .unwrap();

        let modest_balance = vec![Balance::from(coins(6, "atom"))];
        let msg_transfer_a_little = ExecuteMsg::MoveBalances {
            balances: modest_balance,
            account_id: Addr::unchecked("alice")
        };

        let task = TaskRequest {
            interval: Interval::Once,
            boundary: Boundary { start: None, end: None },
            stop_on_fail: false,
            action: CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: contract_addr.clone().into_string(),
                funds: vec![],
                msg: to_binary(&msg_transfer_a_little).unwrap(),
            }),
            rules: None
        };
        let msg_create_task = ExecuteMsg::CreateTask {
            task
        };

        let send_funds = coins(6, NATIVE_DENOM);
        let mut res = app.execute_contract(Addr::unchecked(PARTICIPANT), contract_addr.clone(), &msg_create_task, send_funds.as_ref())
            .unwrap();

        let task_hash = res.events[1].attributes[2].clone().value;
        println!("Task hash {}", task_hash);

        // (trying to) Get task by calling query_get_task (see tasks.rs)
        let msg_query_task = QueryMsg::GetTask { task_hash };
        // This fails but I cannot seem to find how to query instead of execute
        res = app.execute_contract(Addr::unchecked(PARTICIPANT), contract_addr.clone(), &msg_query_task, &[]).unwrap();
        println!("Result {:?}", res);

        // TODO: other stuff to do after figuring out query
        // - agent needs to be in pending list
        // - agent needs to be next in line
        // - agent within threshold of acceptance timeline
        // - can accept
        // - promoted to active
        // - gets removed if next nomination completes successfully
    }
}
